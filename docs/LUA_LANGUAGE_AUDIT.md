# Lua 5.4 language audit

Where Moonseed stands against the Lua 5.4 language: its grammar, the rules the compiler enforces, the compiler's limits, and the official Lua 5.4.9 test suite. Phase 3.20 wrote it; update it with every change to the language core. `LUA_COMPATIBILITY.md` has the detail of each deviation.

## Statement

The Lua 5.4 source-language core is implemented: every production of the grammar in the reference manual (§9) compiles and runs with Lua 5.4.9's meaning, and so does every scope and control-flow rule the manual states for it. The deviations are the implementation limits below, and one order the manual leaves undefined: which of two fields of a constructor with the same key wins. Library and debug behaviour is not part of this statement, and Moonseed does not claim to be compatible with Lua 5.4.

The evidence:
- A grammar corpus of 105 programs, 63 legal and 42 illegal, covering every production and the lexical forms. It is given the same acceptance and results as Lua 5.4.9, runtime errors included, apart from the constructor order and two programs that read library globals (`type`, `math`).
- The Lua 5.4.9 fixtures in the test suite (`NATIVE_FIXTURES`, `CONTROL_FIXTURES`, `DEEP_FIXTURES`).
- 9,000 generated `goto` programs.
- All 33 files of the official suite compile.

## Grammar

| Production | Status |
|---|---|
| `chunk`, `block` | SUPPORTED. A chunk is a vararg function |
| `stat ::= ';'` | SUPPORTED |
| `varlist '=' explist` | SUPPORTED. Targets are recorded before the stores, which run right to left |
| `functioncall` as a statement | SUPPORTED |
| `label` (`::Name::`) | SUPPORTED |
| `break` | SUPPORTED |
| `goto Name` | SUPPORTED, with visibility, duplicate, scope, and trailing-label rules (ADR 0030) |
| `do block end` | SUPPORTED |
| `while exp do block end` | SUPPORTED |
| `repeat block until exp` | SUPPORTED; the condition sees the body's locals |
| `if exp then block {elseif exp then block} [else block] end` | SUPPORTED |
| `for Name '=' exp ',' exp [',' exp] do block end` | SUPPORTED |
| `for namelist in explist do block end` | SUPPORTED, with the closing value |
| `function funcname funcbody` | SUPPORTED |
| `local function Name funcbody` | SUPPORTED |
| `local attnamelist ['=' explist]` | SUPPORTED |
| `attrib ::= ['<' Name '>']`: `<const>`, `<close>` | SUPPORTED |
| `retstat ::= return [explist] [';']` | SUPPORTED; last in its block, as in Lua |
| `funcname ::= Name {'.' Name} [':' Name]` | SUPPORTED |
| `var ::= Name \| prefixexp '[' exp ']' \| prefixexp '.' Name` | SUPPORTED |
| `namelist`, `explist` | SUPPORTED, with Lua's result adjustment |
| `exp ::= nil \| false \| true` | SUPPORTED |
| `Numeral` | SUPPORTED: decimal and hexadecimal integers and floats, with exponents |
| `LiteralString` | SUPPORTED: short strings with every escape, long brackets of any level |
| `'...'` | SUPPORTED |
| `functiondef` | SUPPORTED |
| `prefixexp ::= var \| functioncall \| '(' exp ')'` | SUPPORTED |
| `tableconstructor` | SUPPORTED WITH DOCUMENTED DEVIATION: fields are stored in source order, where PUC Lua stores list fields in batches after the keyed ones. The manual leaves the order undefined; it shows only when two fields have the same key |
| `exp binop exp` | SUPPORTED: all 21 binary operators, with Lua's precedence and associativity |
| `unop exp` | SUPPORTED: `-`, `not`, `#`, `~` |
| `functioncall ::= prefixexp args \| prefixexp ':' Name args` | SUPPORTED; a method call evaluates its receiver once |
| `args ::= '(' [explist] ')' \| tableconstructor \| LiteralString` | SUPPORTED |
| `funcbody`, `parlist` | SUPPORTED: fixed parameters, `...` last, `self` for `:` |
| `fieldlist`, `field`, `fieldsep` | SUPPORTED: `[exp] = exp`, `Name = exp`, `exp`, with `,` or `;` and a trailing separator |
| Comments, `--[==[ ]==]` | SUPPORTED |
| A first line starting with `#` | LIBRARY-DEPENDENT: `luaL_loadfile` skips it; `load` does not, and neither do Moonseed's `load` and `compile` |

Rules the compiler enforces, as Lua 5.4.9 does:
- a local's scope;
- `...` only in vararg functions;
- no assignment to a `<const>` or `<close>` local, including through any number of upvalues;
- one `<close>` per list;
- one attribute per name;
- `goto` visibility, duplicate labels, and no jump into a local's scope;
- `break` only in a loop;
- `return` last in its block;
- proper tail calls exactly for `return f(...)` outside `<close>` scopes.

Behaviour that depends on a library or the debug interface, not on the language:

| Behaviour | Depends on |
|---|---|
| `loadfile`, `dofile`, and `require` of files | a filesystem or module-resolver capability; `require` with `package.preload` and `load` exist (ADR 0039, ADR 0031) |
| string methods (`("x"):len()`), which need the string metatable | the string library |
| local names, line numbers, tracebacks, hooks | the debug library (ADR 0040, ADR 0059 for hooks) |

`<const>` locals compile to ordinary locals. PUC Lua folds the last `<const>` local of a declaration whose initializer is a literal into a compile-time constant; Moonseed keeps its register but gives it no debug name (Phase 3.34), so `debug.getlocal` and error messages match PUC.

## Compiler and runtime limits

Every limit fails deterministically, with a `Limit` compile error or a catchable Lua error, never an overflow or a panic. The compile limits are tested at their exact boundaries (`frontend_limits_hold_at_their_boundaries`, `every_register_window_stops_at_the_limit`), and long chains of every operator and suffix on a 2 MiB thread stack (`long_chains_are_limit_errors_not_stack_overflows`).

| Limit | Moonseed | PUC Lua 5.4.9 | Error | Must match? |
|---|---|---|---|---|
| Source size | 16 MiB by default (`CompileLimits`, up to 1 GiB) | none | `Limit` | no |
| One string literal | the source limit (since Phase 3.30) | none | `Limit` | no |
| One numeral | none since Phase 3.24: the source limit | none | — | yes |
| Parser nesting | 200 levels, two per parenthesis: 98 nested parentheses | 200 levels: about 198 parentheses | `Limit` | no; deeper nesting is rare |
| Operator and suffix chains | one parser level per left-associative operator (`a + b + c`) or suffix (`.a`, `[k]`, `()`, `:m()`), so about 190 links along one path of an expression | none: PUC Lua emits code as it parses | `Limit` | no. Moonseed compiles from a syntax tree, and compiling or dropping the tree recurses on it; before Phase 3.20, 8,000 links overflowed a 2 MiB thread stack |
| Nested functions | 64, and within the parser's nesting: 39 of `(function() return … end)()` | 200 syntax levels | `Limit` | no |
| Locals in one function | 200 | 200 | `Limit` | yes; same |
| Upvalues of one function | 200 | 255 | `Limit` | no |
| Registers | 250 | 255 | `Limit` | no |
| Functions directly inside one function | the chunk's function limit (since Phase 3.30) | 131,071 | `Limit` | no |
| Functions in one chunk, its own included | 65,536 by default (`CompileLimits`, up to 2^20) | none | `Limit` | no |
| Constants of one function | 2^20 by default (up to 2^24) | 33,554,431 | `Limit` | no |
| Instructions of one function | 2^20 by default (up to 2^24) | none in practice | `Limit` | no |
| Jump distance | the instruction limit (32-bit offsets since Phase 3.30) | 16,777,215 | `Limit` | no |
| Lua call depth | 1,000 frames (1,080 with error handling) | bounded by 1,000,000 stack slots: about 1,000,000 calls of a function with one register, 111,109 of one with eight | catchable "stack overflow" | no; documented |
| Stack slots per thread | 50,000 by default, 1,024–100,000 | 1,000,000 | catchable "stack overflow" | no |
| `__call` chain | 200 | stack-bound | catchable error | no |
| Heap | 64 MiB logical quota by default | none | catchable "not enough memory" | no |
| Live objects | 1,048,576 by default (up to 2^28) | none | catchable "not enough memory" | no |
| One string | the quota by default (`max_string_bytes`, up to 1 GiB) | none | catchable memory error | no |
| Snapshot | 128 MiB by default (`max_snapshot_bytes`); a full default heap fits | n/a | `SnapshotError::LimitExceeded` | Moonseed only |

No file of the official suite reaches a compile limit. Phase 3.30 raised the compile limits that were low for real programs, with the snapshot and binary-chunk limits that must accept what they compile (ADR 0052).

## Error messages

Moonseed does not promise PUC Lua's error text. Differences, by class:
- **Same class, different message:** runtime errors say what happened ("attempt to call a non-callable value") without the variable's name.
- **Missing variable name:** calls, indexing, and arithmetic on nil do not name the variable. The debug information that would name it exists since Phase 3.24 (ADR 0040); errors do not use it yet.
- **Missing source position:** runtime errors carry no `chunk:line:` prefix, though each instruction's line is known and tracebacks show it. Compile errors carry a byte span, which `line_col` turns into a line and column.
- **Different limit:** above.
- **Semantic mismatch:** none known.

## The official Lua 5.4.9 test suite

`lua-5.4.9-tests.tar.gz` from `https://www.lua.org/tests/`, SHA-256 `7d971845f545ffc09fbb3128a86b2c6524161c70d0fdf0154a16e8c00c343fca`, MIT licensed like Lua. It is not in the repository. `tools/lua_suite.sh` fetches it into `.cache/` (gitignored), checks the archive's hash on every run and extracts it afresh, then runs `moonseed-compat` over each file under process caps. The results are in `tests/compat/lua54-baseline.json`, with the hash the script checked; the harness run by hand on another directory records none.

The harness:
- **Loading:** loads each file as `luaL_loadfile` does, skipping a first line that starts with `#`, and runs it under Moonseed's default limits with a fuel limit of 200,000,000 instructions.
- **The environment:** Moonseed's standard libraries as `Runtime::install_standard` installs them (the base library, `math`, `table`, and `string`), and nothing else. No global is stubbed. `print` writes to a buffer; the lines it wrote, and the last, are recorded as the file's progress.
- **Recording missing globals:** a `do … end` block in front of line 1. It takes the recorder from a harness global, `__moonseed_harness_note`, clears that global, and runs `setmetatable(_ENV, { __index = note })`. `note` records the name of each missing global that is read and returns nothing, so the name stays nil, and line numbers do not move. The one visible difference is that `getmetatable(_ENV)` is not nil.
- **Classifying a file that ends:** `PASS` only when every missing global it read is not a standard one: the suite reads names such as `undef` on purpose as nil, as PUC Lua does, and `Message`, which `all.lua` defines, is replaced with `print` in a file read alone. A file that also read `T`, PUC's C test library, and ends is `COMPLETED_WITHOUT_T`: it skipped what needs `T`, as it does in PUC Lua built without it. Any other file that ends having read a missing global is `COMPLETED_WITHOUT` it, because a `pcall` may have caught the error that global's absence raised.
- **Progress:** how many lines `print` wrote, and the last one.
- **Classifying a failure:** by the last missing global read before the error. This labels a file; it does not prove a cause. A failure that comes from the language core, after the file has read some missing global, is labelled with that library, and 32 of the 33 files read `print`. So `UNKNOWN` is a lower bound on core failures, not a detector. Core discrepancies are found by the fixtures and corpora above, and by reading each suite file's failure by hand once the libraries it needs exist.

Baseline, Phase 3.20 (`tests/compat/lua54-baseline.json`):
- **Compiles:** 33 of 33 files, all with Moonseed's compiler as it is.
- **Passes:** 0 of 33. Every file stops within its first lines on a missing library, before reaching the code it tests:

| Last missing global read before the error | Files |
|---|---|
| `print` | 24 |
| `require` | 3 |
| `io` | 2 |
| `collectgarbage` | 2 |
| `math` | 1 |
| `string` | 1 |

- **UNKNOWN:** 0. Every file stops at its first lines, before the code it tests: the core-heavy files (`constructs`, `locals`, `vararg`, `closure`, `calls`, `literals`, `bitwise`, `code`, `events`) at `print`, and `goto` at `collectgarbage`. So the suite has not yet tested Moonseed's core semantics. It will once the base library runs.

Bootstrap dependencies, by the number of suite files that name each global:

| Global | Files |
|---|---|
| `print` | 32 |
| `assert` | 31 |
| `string` | 30 |
| `math` | 25 |
| `load` | 22 |
| `require` | 21 |
| `table` | 19 |
| `_G` | 18 |
| `debug`, `collectgarbage` | 17 each |
| `type`, `coroutine`, `pairs` | 16 each |
| `T` (the internal test library, a PUC-only build) | 13 |
| `io` | 12 |
| `os` | 11 |
| `arg`, `tonumber` | 10 each |
| `tostring` | 9 |
| `next` | 8 |
| `ipairs` | 7 |
| `warn` | 4 |
| `dofile`, `loadfile`, `package` | 3 each |
| `_VERSION`, `utf8` | 2 each |

`all.lua` itself names `_VERSION`, `io`, `_G`, `arg`, `debug`, `T`, `print`, `math`, `string`, `os`, `collectgarbage`, `assert`, `type`, `table`, `dofile`, `loadfile`, `load`, `require`, `coroutine`, `warn`, `tonumber`, `pairs`, and `tostring`. It stops first because `_VERSION` is missing: its version check fails, and the message it then writes needs `io`.

Phase 3.21, with the base library (`tests/compat/lua54-phase321.json`; `tools/lua_suite_diff.py` compares two runs):
- **Compiles:** 33 of 33.
- **Passes:** 0 of 33. `api.lua` and `code.lua` are `COMPLETED_WITHOUT_T`: they skip themselves whole without `T`, as they do in PUC Lua built without it. `heavy.lua` is `COMPLETED_WITHOUT` `math`: its `pcall` caught the error a missing `math` raised, where it expects a memory error.
- **Further than before:** 27 files now stop at a later global than in Phase 3.20. Six stop where they did, after the same instructions: `cstack`, `db`, and `files` at `require`, `bwcoercion` at `math`, `tpack` at `string`, and `tracegc` at `io`.
- **The new blockers**, by the last missing global read before the error or the end:

| Blocker | Bucket | Files |
|---|---|---|
| `require` | PACKAGE LIBRARY | 16 |
| `math` | MATH LIBRARY | 6 (with `heavy`) |
| `string` | STRING LIBRARY | 3 |
| `table` | TABLE LIBRARY | 2 |
| `os`, `io` | IO / HOST CAPABILITY | 3 |
| `T` | USERDATA/C API | 2 (completed, skipping themselves) |
| none: the fuel limit | RESOURCE LIMIT | 1 (`closure`) |

- **`closure.lua`** runs its first tests, which pass: 1,000 closures sharing upvalues, and the explicit collections. It then spins in `while x[1] do … end` until a table whose metatable has `__mode = 'kv'` loses its entry to a collection. Moonseed has no weak tables, so the loop never ends and the file stops at the fuel limit. This is a GC-track gap, not a core failure.
- **`require`:** 13 of the 16 begin with `local debug = require "debug"` (or `'debug'`), `bitwise.lua` with `require "bwcoercion"`, `utf8.lua` with `require'utf8'`, and `attrib.lua` tests `require` itself. A real `require` alone would move 13 of them to "module 'debug' not found": what they need first is the package library and the debug library.
- **UNKNOWN:** 0, and no failure is in the language core or in a base function added in Phase 3.21. The files still stop within their first lines: the most any file ran, `closure` aside, is 3,480 instructions (`nextvar`). The core semantics are still mostly untested by the suite.

Phase 3.22, with `math` and `table` (`tests/compat/lua54-phase322.json`):
- **Compiles:** 33 of 33.
- **Passes:** 0 of 33 outright. `vararg.lua` runs to its end and prints `OK`. The harness labels it `COMPLETED_WITHOUT` `arg`, because it reads the standalone interpreter's `arg`, but it only checks `arg == _G.arg`, which holds when both are nil. `heavy.lua` runs its whole memory test, 9.4 million instructions: the table it fills until memory runs out, and the `pcall` that catches that. It then reads `io`.
- **The six `math` blockers:** all passed `math`. `all`, `bwcoercion`, `math`, `nextvar`, and `strings` now stop at `string`, and `heavy` completes. **The two `table` blockers:** `sort` stops at `string`; `vararg` completes.
- **Further than before:** 8 files. `math.lua` runs 484 instructions before `string`, where it ran 18; `nextvar.lua` 6,497, where it ran 3,480.
- **The new blockers:**

| Blocker | Bucket | Files |
|---|---|---|
| `require` | PACKAGE LIBRARY | 16 |
| `string` | STRING LIBRARY | 9 |
| `io`, `os`, `arg` | IO / HOST CAPABILITY | 5 (two of them complete) |
| `T` | USERDATA/C API | 2 (completed, skipping themselves) |
| none: the fuel limit | RESOURCE LIMIT | 1 (`closure`, the weak table) |

- **UNKNOWN:** 0, and no failure is in the language core, the base library, `math`, or `table`.

Phase 3.23, with `string` (`tests/compat/lua54-current.json`):
- **Compiles:** 33 of 33.
- **Passes:** 2. `tpack.lua` (the pack functions) and `bwcoercion.lua` (string coercion through the string metatable).
- **The nine `string` blockers:** all passed `string`. 9 files went further; `nextvar.lua` ran 340,899 instructions, where it ran 6,497, and `math.lua` 16,341, where it ran 484.
- **Every failure reached in the string library was investigated** by running the files with each `assert` and `checkerror` reporting its line:
  - `strings.lua`: `string.rep` past the size limit raised "not enough memory" where the test expects "resulting string too large"; it now raises Lua's error. `%p` of two equal short strings differed, where Lua interns them; a short string's `%p` token now comes from its bytes. The file now stops at `coroutine.running()`. Run with that and `os.setlocale` stubbed, the rest of its string checks pass; what remains is `table.concat`'s argument wording.
  - `pm.lua` stops at `utf8.charpattern`. With `utf8` stubbed, every check passes that passes in Lua 5.4.9 with the same stub.
  - `tpack.lua` passes.
- **The new blockers:**

| Blocker | Bucket | Files |
|---|---|---|
| `require` | PACKAGE LIBRARY | 17 (13 for `debug`; `string`, `utf8`, and the suite's `bwcoercion` and `tracegc` modules) |
| `package` | PACKAGE LIBRARY | 2 (`all`, `nextvar`) |
| `io`, `os`, `arg` | IO / HOST CAPABILITY | 5 (two of them complete) |
| `utf8` | UTF8 LIBRARY | 1 (`pm`) |
| `coroutine` | COROUTINE LIBRARY | 1 (`strings`) |
| none: an assertion | UNKNOWN | 2 (`sort`, `math`) |
| `T` | USERDATA/C API | 2 (completed, skipping themselves) |
| none: the fuel limit | RESOURCE LIMIT | 1 (`closure`, the weak table) |

- **UNKNOWN:** 2, neither in the string library. `sort.lua` stops at its first `checkerror`: the table library's argument errors still carry Moonseed's generic "bad argument to a base function", where the test matches Lua's text ("wrong number of arguments"). `math.lua` stops at a `checkerror` on the core's "attempt to perform 'n//0'" wording; past it, `tonumber` of hex numerals with 1,000 digits fails, and the math library's argument messages are generic too.

Phase 3.24, with `package`, `require`, and `debug` (`lua54-phase324.json`; `lua54-phase323.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 2, as before.
- **`require`:** no file stops on it. The harness now installs `debug` and names each file `@<file>`, so tracebacks name the file and line where a file stops.
- **The 13 `require "debug"` files** all go further and none passes. Each stops on:
  - the coroutine library: `calls.lua` (line 187), `coroutine.lua` (line 10);
  - debug hooks: `db.lua` (`debug.gethook`, line 16);
  - `debug.upvalueid`: `goto.lua` (line 188);
  - compile or lexer error wording: `constructs.lua` (line 242, "unknown attribute 'XXX'"), `errors.lua` (line 65, a syntax error's text), `literals.lua` (line 83, "near ..." of an escape error);
  - `io` or `os`: `events.lua` (`io.stdin`, line 196), `files.lua` (line 8);
  - `collectgarbage` modes: `gc.lua`, `gengc.lua`;
  - the heap quota: `big.lua` ("not enough memory" filling large tables);
  - a suite module: `locals.lua` (`require "tracegc"`, line 8).
- **Found on the way:** Moonseed counted only `\n` as a line break; `literals.lua`'s `lexstring` checks showed that `\r`, `\r\n`, and `\n\r` break lines in Lua too. Fixed.
- **The new blockers:**

| Blocker | Bucket | Files |
|---|---|---|
| `io`, `os`, `arg` | IO / HOST CAPABILITY | 9 (two of them complete) |
| `coroutine` | COROUTINE LIBRARY | 4 (`calls`, `coroutine`, `nextvar`, `strings`) |
| none: an error's wording | UNKNOWN | 4 (`constructs`, `errors`, `literals`, `math`) |
| a missing module | UNKNOWN | 5 (`attrib` for `io`, `bitwise` for `bwcoercion`, `cstack` and `locals` for `tracegc`, `utf8` for `utf8`) |
| hooks, `upvalueid` | UNKNOWN | 2 (`db`, `goto`) |
| `collectgarbage` modes | UNKNOWN | 2 (`gc`, `gengc`) |
| the heap quota | UNKNOWN | 1 (`big`) |
| `utf8` | UTF8 LIBRARY | 1 (`pm`) |
| `T` | USERDATA/C API | 2 (completed, skipping themselves) |
| none: the fuel limit | RESOURCE LIMIT | 1 (`closure`, the weak table) |

- **`sort.lua`** now passes its argument-error checks and stops at `os.clock`; **`math.lua`** passes the numeral and `n//0` checks and stops at "field 'huge'", a runtime error's variable name.

Phase 3.25, with `coroutine` (`lua54-phase325.json`; `lua54-phase324.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 2, as before.
- **The four `coroutine` files** all go further: `nextvar.lua` and `strings.lua` stop at `io.stdin`; `calls.lua` at a compile error's wording ("unexpected symbol", line 354); `coroutine.lua` at `debug.sethook` (line 349).
- **Every failure reached in the coroutine library was investigated.** `coroutine.lua` first stopped at line 238: a `__close` run by an error's unwind inside `pcall` saw the failing frame as its caller, where Lua has popped it and shows `pcall`. Debug levels now hide the frames an unwind is leaving. With hooks stubbed and each `assert` reporting its line, the file runs to its end with two failures, neither in the library: the hook trace (line 352) and a weak table that is never cleared (line 417). `calls.lua`, `nextvar.lua`, and `strings.lua` report no coroutine failure before their blockers.
- **The new blockers:** `io`, `os`, `arg` (11, two of them complete); an error's wording (5: `calls`, `constructs`, `errors`, `literals`, `math`); a missing module (5); hooks (`coroutine`, `db`) and `upvalueid` (`goto`); `collectgarbage` modes (`gc`, `gengc`); the heap quota (`big`); `utf8` (`pm`); the C test library (2); the fuel limit (`closure`, the weak table).

Phase 3.26, with userdata (`lua54-phase326.json`; `lua54-phase325.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 3: **`goto.lua`** now passes, its one blocker having been `debug.upvalueid`.
- **No other file moved.** The suite makes userdata only through PUC's C test library (`T.newuserdata` in `gc.lua` and `gengc.lua`) and `io` file handles (`db.lua`'s `getuservalue` checks use `io.stdin`), so the rest of what this phase added is out of its reach for now. `closure.lua`'s `upvalueid` and `upvaluejoin` checks (lines 239–288), which its weak-table wait keeps it from reaching, pass when run alone, the `gmatch` iterator's included.
- **The blockers** are otherwise Phase 3.25's: `io`, `os`, `arg` (11); an error's wording (5); a missing module (5); hooks (`coroutine`, `db`); `collectgarbage` modes (`gc`, `gengc`); the heap quota (`big`); `utf8` (`pm`); the C test library (2); the fuel limit (`closure`, the weak table).

Phase 3.27, with weak tables, ephemerons, finalizers, and `warn` (`lua54-phase327.json`; `lua54-phase326.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 4: **`closure.lua`** now passes; it used to wait until the fuel ran out for a weak table to be cleared. Its `upvalueid` checks pass too.
- **`gc.lua` and `gengc.lua`** still stop at once, at `collectgarbage("incremental")`. With the mode options stubbed and each `assert` reporting its line, `gengc.lua` runs to its end with no failure, and `gc.lua` runs to its end with one: line 201, which expects more incremental steps for a smaller step size, where Moonseed's `step` is one full collection. Three of its loops pass Moonseed's limits and were scaled down to get past them: 4 MiB strings in weak tables (line 470; the 1 MiB string limit), a 200,000-table list, and 1,000 suspended threads alongside the rest of the file (the 10,000-object limit). Its weak-table, ephemeron, finalizer, resurrection, and closing-state sections all pass; the userdata ones are skipped without `T`.
- **`nextvar.lua`** runs slightly further (fuel 346,774 against 346,659), still stopping at `io.stdin`.
- **The blockers** are otherwise Phase 3.26's: `io`, `os`, `arg` (11); an error's wording (5); a missing module (5); hooks (`coroutine`, `db`); `collectgarbage` modes (`gc`, `gengc`); the heap quota (`big`); `utf8` (`pm`); the C test library (2).

Phase 3.28, with the incremental collector (`lua54-phase328.json`; `lua54-phase327.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 4, as before. Every file's outcome is unchanged; fuel rose wherever a file allocates, since collector work is now charged.
- **`gc.lua`** gets past `collectgarbage("incremental")` (line 12) and stops at `collectgarbage("generational")` (line 15), Phase 3.29's. With the generational option stubbed and each `assert` reporting its line, it runs to its end with no failure: line 201, which compares the steps a cycle takes at two step sizes, now holds. Three of its loops pass Moonseed's limits and are scaled down: two 4 MiB strings (the 1 MiB string limit), a 200,000-node list and 1,000 coroutines (the 10,000-object limit). The 4 MiB block also found that a string key in a table weak both ways outlived its entry by a collection, which Moonseed's earlier collector did too; it now goes with its entry, as in Lua.
- **`gengc.lua`** still stops at `collectgarbage("generational")`.
- **The blockers** are otherwise Phase 3.27's: `io`, `os`, `arg` (11); an error's wording (5); a missing module (5); hooks (`coroutine`, `db`); `collectgarbage("generational")` (`gc`, `gengc`); the heap quota (`big`); `utf8` (`pm`); the C test library (2).

Phase 3.29, with generational collection, the default (`tests/compat/lua54-current.json`; `lua54-phase328.json` is the run before):
- **Compiles:** 33 of 33. **Passes:** 4, as before. Fuel changed wherever a file allocates, since young collections replace incremental cycles by default.
- **`gengc.lua`** runs to its end and prints `OK`. Its own message notes that the C test library is not there, so it skips the checks that need `T.gcage`, as it does on a PUC build without it. Classified as completed without `T`.
- **`gc.lua`** passes its mode checks (lines 15–18) and stops at line 471, a table keyed by two strings of 4 MiB, past Moonseed's 1 MiB string limit (a Moonseed hard limit). With each `assert` reporting its line and three loops scaled under the string and object limits, as in Phase 3.28, it runs to its end with no failure and real mode switches.

Phase 3.30 (the scalability envelope):

- **`gc.lua`** runs to its end, classified as completed without `T`: its 4 MiB strings and its 200,000-node list now fit Moonseed's default limits, with nothing scaled down.
- **`big.lua`** gets past its old blocker and stops later, at a missing `undef`.
- **The blockers** are otherwise Phase 3.28's: `io`, `os`, `arg` (11); an error's wording (5); a missing module (5); hooks (`coroutine`, `db`); the heap quota (`big`); `utf8` (`pm`); the C test library (2). None is in collection.
