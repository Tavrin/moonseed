# Phase 3.32 benchmark protocol (corpus revision 2)

`bench-stable` is portable release optimization, one codegen unit, line-table
symbols, and fat LTO. `bench-native` inherits it and **must** be built through
`bench_build.py --profile bench-native`, which sets `-C target-cpu=native`.
The stable runner clears RUSTFLAGS. Do not compare profiles interchangeably.
Build artifacts and compiler/profile provenance are recorded separately; pass
only the binary from that build to collectors. No performance runtime changes.
`bench_build.py` honors `CARGO_TARGET_DIR` and also accepts `--profile release`
for a default-release comparison; its fallback target remains the lane-M target.

All commands run from the repository root. Build and collector resource caps
are implemented inside the Python tools. Run only one Cargo command at a time.

```sh
python3 tools/bench_build.py --output RESULTS/build.json
W=1 R=9 CORE=4 PUC=/path/to/lua MOONSEED_RUN=/path/to/moonseed-run \
  python3 tools/bench_corpus.py --json RESULTS/corpus.json > RESULTS/corpus.md
python3 tools/callgrind_corpus.py --output RESULTS/instructions \
  --puc /path/to/lua --moonseed /path/to/moonseed-run --divisor 100 --cachegrind
python3 tools/bench_profile_report.py RESULTS/instructions/results.json --output RESULTS/PROFILE.md
python3 tools/bench_embed.py --output RESULTS/embedding  # Moss rows need the private integration/moss harness
```

Wall samples alternate engines, with one warmup and nine measured samples by
default. Medians and nearest-rank p95 include process startup/teardown; Moonseed
also reports internal compile, boot and run medians. `empty` is a startup probe,
excluded from aggregates. PUC `luac -p` is a separate process, not parse-only
measurement. All checksums must match byte-for-byte, including repeated runs.
The paired-ratio table reports the median of adjacent Moonseed/PUC sample
ratios and nearest-rank p10/p90/p95. The historical ratio-of-medians table and
aggregates are retained; the two estimators need not agree under changing load.
Category geomeans overlap where workloads exercise several categories; the
overall geomean gives each nonempty workload one vote. Partial aggregate counts
are explicit. Embedding is measured separately in ns/op; it has no PUC ratio.

Revision-1 Lua files are unchanged. Revision 2 adds branches, array read/write,
global read/write, tail recursion, isolated field writes, bounded concatenation,
and isolated formatting. New workload PUC calibration on 2026-10-02 was
163–379 ms (diagnostic, not a performance-mode baseline).

Environment snapshots include compiler/LLVM, C compiler, declared PUC flags,
CPU/kernel/governor, affinity, load, profile and flags. `BENCH_MAX_LOAD` defaults
to 1.0. Any sample above it invalidates wall acceptance; today all wall evidence
is diagnostic regardless of load (`BENCH_DIAGNOSTIC=1`, default). Disabling that
flag is appropriate only after the owner establishes performance mode. Counts
are load independent, but code layout and cache simulations remain build-specific.

Instruction collectors use Valgrind 3.25.1, whole-program counting, identical
scaled sources for both engines, one pinned core, and explicit substitutions
recorded with original/generated source hashes. Scaling is deterministic and
never modifies the corpus. Fibonacci scales its argument logarithmically;
coroutine producer/consumer bounds scale together; sort scales array size but
keeps four sort pairs. Scaling nested bounds changes more than linear work:
never extrapolate these counts to full workloads by multiplying by the divisor.
Raw callgrind/cachegrind output is retained. Top inclusive and exclusive symbols
come from callgrind_annotate; only exclusive counts may be summed. Subsystem
classification is heuristic and cannot recover inlined attribution.
For other builds, pass `callgrind_corpus.py --profile PROFILE` to record the
supplied binary's profile, and retain its build sidecar with actual flags.

Embedding exposes native 20-sample p95 through the existing `measure` feature;
the ordinary VM build contains none of this instrumentation. Moss runs its
existing replay check and 100/1k/10k frame benchmark with its counting allocator.
Both workspaces use matching profiles. The wait embedding row includes setup,
start, complete and resume in one operation; it does not claim separate timings.

LTO off/thin/fat results and final selection are recorded in the lane results
`baseline/LTO.md`; full baseline and instruction profiles are in that directory.
Fat was selected by the lowest scaled Ir-ratio geomean: off 5.39738, thin
5.34187, fat 5.30013 (21 nonempty workloads, divisor 100). This modest 1.8%
instruction reduction over off is not a wall-speedup claim. Build wall times
were 26.17 / 32.83 / 28.83 seconds respectively under differing load.

Perf became available during collection (`perf_event_paranoid=2`). Use the real
kernel tool, not the broken `/usr/bin/perf` wrapper:

```sh
python3 tools/perf_corpus.py --output RESULTS/perf --puc /path/to/lua \
  --moonseed /path/to/moonseed-run
```

The collector records user-space cycles, instructions, branches, branch misses,
L1 instruction/data load misses, and LLC load misses. Unsupported events remain
explicitly unavailable. Raw CSV includes event running time and multiplexing
percentage. A separate `cycles:u` sampling run (499 Hz) retains perf.data and top
25 symbols per workload and engine. Both runs execute full corpus sources and
check checksums. Perf cycles/misses and all wall times are labelled noisy; perf
instruction totals complement rather than replace Callgrind's repeatable Ir.

The existing embedding and Moss harnesses report the upper middle sample for an
even sample count (indices 10/20 and 25/50). That convention is preserved for
longitudinal comparison. Their p95 is nearest-rank (indices 18/20 and 47/50).
The Python corpus runner uses the arithmetic median. Cache miss rates have
different access-count denominators across engines; use the retained absolute
miss counts before interpreting a smaller percentage as less cache work.
