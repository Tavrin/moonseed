# Warmed call family

These microbenchmarks do not change the frozen `bench/corpus`. Each file runs
unchanged on Moonseed and PUC Lua 5.4.9, warms 1,000 iterations, then checks and
prints a deterministic checksum. `tools/call_bench.py` renders iteration counts
and call/control selection into its output directory, leaving these files intact.

The caller receives its target as a parameter so ordinary-call probes do not
accidentally measure captured-variable lookup. The fixed/vararg matrix describes
the loop's caller and target callee. Upvalue cases describe the target closure.
There are sixteen expressions per iteration; open-results and tail-call probes
make two dynamic Lua invocations per expression. `fib(6)` makes 25. The header
records the actual denominator, checked against the optional execution counters.

Subtract the low/high no-call slope from the low/high call slope. Controls retain
the useful arithmetic but remove the call and its argument/result plumbing.
The family table therefore includes differing setup and callee-body instructions;
it is not an intrinsic Call/Return ABI table. The fixed 0/0 and 1/1 address
decompositions separately remove supporting Moves and their dispatch.

Build sequentially with the lane's Cargo target and caps:

```sh
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build --profile bench-stable -p moonseed-bench --bin moonseed-run )
python3 tools/call_bench.py --output results/calls/family --perf
python3 tools/call_bench.py --output results/calls/exact --iterations 1000 10000 --benchmarks fixed_00 fixed_11 --dump-instr
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build --profile bench-stable -p moonseed-bench --features alloc-gc --bin call-alloc )
```

`call-alloc` installs the two optional native markers and counts host allocations
only after warmup and before checksum/output. Its end marker has fixed allocation
overhead; use both iteration counts and the no-call control to identify per-call
allocations. Build the allocation probe without `counters`, since counter-map
initialization itself allocates. A separate `alloc-gc,counters` build checks dynamic
call counts, fuel, and collector activity. These features are benchmark-only.

The collector caps and pins each child run, saves commands, exit status, checksums,
source/binary hashes and load averages. It reports perf failures explicitly.
Cycles and nanoseconds on the shared machine are diagnostic. Instruction-address
accounting uses `tools/call_bench_decompose.py`, which verifies that exclusive
instruction counts sum to each Callgrind total before control subtraction.
The address-range map must be reviewed for the exact binary being decomposed.
