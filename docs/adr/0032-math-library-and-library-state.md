# ADR 0032 — The math library: one portable float backend, and the random generator as snapshot state

## Context

The official suite's next blockers after Phase 3.21 were `math` (6 files) and `table` (2).

`math` raises two questions no earlier library did.

**Transcendental functions.** Moonseed runs the same program native and on wasm32 and compares the results (D1). Until now `^` was the only transcendental operation, and it used `f64::powf`. That calls the host C library natively (glibc here) and a musl port on wasm32. The two agree to within one unit in the last place, not to the bit.

Measured on 20,030 inputs per function against glibc, `libm` differs by at most 1 ulp:

| Function | Inputs that differ |
|---|---|
| `sin`, `cos`, `tan`, `exp`, `log10` | 1.6–2% |
| `pow` | 0.9% of 114,480 pairs |
| `atan2` | 8% of pairs |
| `sqrt`, `floor`, `ceil`, `fmod` | never; they are exact |

**`math.random`.** Lua 5.4 specifies xoshiro256**, seeded by `math.randomseed(x, y)` or, with no argument, from the time and an address. Its state is hidden in a userdata that `random` and `randomseed` capture. Moonseed has no userdata. A seed from the clock would make a run depend on when it happened.

## Decision

### One float backend

Every transcendental function, and `^`, uses the `libm` crate, a pure-Rust port of musl's libm, on every target:
- the `math` functions `sin`, `cos`, `tan`, `asin`, `acos`, `atan` (`atan2`), `exp`, `log`, `log2`, `log10`, `sqrt`, `floor`, `ceil`, `fmod`;
- `^`, which used `f64::powf` before.

`libm` is MIT licensed and has no dependencies. It is used with no default features, since its `arch` feature swaps in target intrinsics. It is Moonseed's first dependency, added because no std function gives the same bits on every target.

The result: the same inputs give the same bits native and on wasm32. `math_bits_fingerprint` checks this without decimal formatting in between, over 2,000 inputs for every function called directly, and 64 of them through the runtime. The sign and payload of a NaN are not part of the contract; the fingerprint folds every NaN as one value.

Against Lua 5.4.9 on this machine, results may differ from glibc's by 1 ulp. The math corpus nonetheless prints identically at `%.14g`. The oracle test allows a relative difference of 1e-13 on transcendental lines only, on at most 20 lines.

Speed against glibc:

| Function | Cost |
|---|---|
| `sin`, `cos`, `atan2` | faster |
| `log` | 1.3 times |
| `exp` | 1.8 times |
| `pow` | 3 times (49 ns against 16 ns) |

`PERFORMANCE.md`, Phase 3.22.

### The math functions

The `math` table has every Lua 5.4.9 function and constant: `abs`, `acos`, `asin`, `atan`, `ceil`, `cos`, `deg`, `exp`, `floor`, `fmod`, `log`, `max`, `min`, `modf`, `rad`, `random`, `randomseed`, `sin`, `sqrt`, `tan`, `tointeger`, `type`, `ult`, `pi`, `huge`, `maxinteger`, `mininteger`. Lua 5.4.9's normal build has no deprecated aliases, so neither does Moonseed.

They follow `lmathlib.c`:
- **Argument checks:** `luaL_checknumber` and `luaL_checkinteger`, through the conversions arithmetic uses (`lex::string_to_number`, `base::lua_integer`).
- **Integers stay integers:**
  - `abs` of an integer wraps: `abs(mininteger)` is `mininteger`.
  - `floor` and `ceil` return an integer when the result fits one.
  - `modf` returns a float fraction and rounds toward zero.
  - `fmod` of two integers is the C remainder, with a zero divisor an error and `mininteger % -1` giving 0.
- **`log`** uses `log2` for base 2, `log10` for base 10, and `log(x) / log(b)` otherwise, as Lua does.
- **`min` and `max`** return one of their arguments, chosen with `<`. That may call `__lt` (ADR 0033).
- **`math.type`** returns reserved strings.
- Errors are `Argument` errors.

### The generator

`LibraryState { rng: [u64; 4], entropy: u64 }` is heap state, written to snapshots after the collector's state.
- **Generator:** `rng` is Lua 5.4.9's xoshiro256**. `random` advances it before checking its arguments, as Lua does.
- **Floats:** a float is the top 53 bits of an output, scaled into `[0, 1)` (`I2d`).
- **Integer ranges:** `project`, which masks to the smallest `2^b - 1` above the range and draws again when the value falls outside, so there is no modulo bias.
- **Seeding:** `randomseed(x, y)` is Lua's `setseed`. It puts the seeds and `0xff` in the state, discards sixteen outputs, and returns the two seeds.

The sequences from explicit seeds are Lua 5.4.9's: 1,506 lines of outputs across every argument form (`corpus_random.lua`).

`entropy` is a deterministic stream, a splitmix64 counter started from `Config::entropy` (default 0). It seeds the generator in two cases:
- when `Runtime::install_math` installs the library;
- when `math.randomseed()` gets no argument and the host has set no entropy.

`Runtime::set_entropy` gives a host source instead, for `randomseed()` only. Each of its two words is an external effect committed through the journal, so a replay reads the words back and does not ask the host again. The source is host state, like the output, and a host sets it again after a restore.

`randomseed()` with no argument takes two effect sequence numbers either way, so later effect ids do not depend on whether a source was set. Moonseed never reads the clock or an address.

Restore refuses an all-zero generator state, which xoshiro cannot leave and seeding never makes.

This is the one place library state lives. It is not in any Lua table, so iteration cannot see it. A later stateful library adds a field here, not a new mechanism.

### Installation

- `register_math`, `register_table`, and `register_standard` register the functions.
- `Runtime::install_math`, `install_table`, and `install_standard` (base, math, and table) install them.
- A sandbox installs only what it wants.
- The `math` and `table` globals are ordinary tables, bound as globals before they are filled.

## Alternatives

- **The host's C library:** faster `pow` and `exp`, but native and wasm32 results would differ, and so would results across hosts' C libraries.
- **A userdata or native closure for the generator:** a new value kind only for this.
- **Seeding from the clock when no host entropy is given,** as PUC Lua does: a run would depend on when it started, and a replay could not reproduce it.
- **A `rand` crate generator:** not Lua's sequence.

## Consequences

**Determinism.** `DETERMINISM.md` now covers transcendental functions and `^` at D1. `^` results changed in the last bit for about 1% of inputs, from glibc's to musl's; snapshots from before are refused anyway (schema 12).

**Revisions.**

| Revision | Value | Why |
|---|---|---|
| Snapshot schema | 12 | library state, reserved `integer` and `float`, new error classes |
| Bytecode | 11 | unchanged |
| Tables | 3 | unchanged |
| Fuel | 3, with ADR 0033 | |
| GC policy | 4, with ADR 0033 | |

**Compatibility.** `LUA_COMPATIBILITY.md` lists what differs: error messages, the default seed, and `%.14g` output where glibc and musl differ by an ulp near a rounding edge.
