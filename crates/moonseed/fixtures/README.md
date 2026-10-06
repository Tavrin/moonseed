# Package-local proof fixtures

`lua/` holds the small Lua fixture suite used by the unit tests and the oracle
comparisons. `civil/` and `diag/` are copies of the civil-time records and the
diagnostic determinism sample from the workspace corpora. Compile-time and runtime fixture paths remain inside this crate so its
published archive can build and run its unit suite independently.

The workspace corpora under `tests/` remain authoritative for the `civil/` and
`diag/` copies; update both when they change. Large diagnostic, hook, UTF-8
and host-library corpora, suite ledgers and benchmark dumps are not packaged.
