# Lua compatibility

Moonseed implements the Lua 5.4 source language and Lua-visible runtime semantics, with a broad standard library; no Lua C API, PUC binary chunks, arbitrary C modules; host IO/OS is capability-based. This file records rules that are deliberately different from, or narrower than, Lua 5.4. The optional `MOONSEED_LUA54` test runs a local Lua 5.4.9 binary built without `LUA_COMPAT_5_3`. The stock Makefile turns that on, and with it Lua 5.3's `__le` fallback, which the 5.4 manual removes; the test refuses such a binary. PUC Lua is not a dependency and CI does not run it.

## Source

| Piece | Status |
|---|---|
| Lexical tokens, comments, strings, numerals | Lexed. Differential accept/reject and a set of literal values against Lua 5.4.9 |
| `local`, name assignment, `return`, calls, `function (...) end`, parentheses | Parsed, compiled, run. The closure fixture matches hand bytecode and Lua's returned `1, 1, 2, 2` |
| `+ - * / // % ^`, `& \| ~ << >>`, unary `-` and `~`, `..` | Parsed, compiled, run, with every metamethod, and numeric strings in arithmetic through the string metatable. Arithmetic, bitwise, concatenation, and metamethod fixtures match Lua 5.4.9 |
| `if` / `elseif` / `else`, `do ... end` | Parsed, compiled, run. Fixtures match Lua 5.4.9 |
| `while`, `repeat ... until`, `break` | Parsed, compiled, run. Fixtures match Lua 5.4.9 |
| `==`, `~=`, `<`, `<=`, `>`, `>=` | With `__eq`, `__lt`, `__le`. Fixtures and 1,200 number-pair comparisons match Lua 5.4.9 |
| Numeric `for`, unary minus | Parsed, compiled, run. Seven `for` fixtures and a unary-minus fixture match Lua 5.4.9 |
| Table constructors, `t[k]`, `t.name`, indexed and field assignment, globals, `_ENV` | Parsed, compiled, run. Eight fixtures match Lua 5.4.9 |
| Native (Rust) functions as values | Stored, passed, compared, and called like Lua functions |
| Table metatables, `__index`, `__newindex`, `__len`, `__call`, operator metamethods, `#`, `setmetatable`, `getmetatable`, `rawget`, `rawset`, `rawlen`, `rawequal` | Implemented. Metatable fixtures match Lua 5.4.9, plus error classes |
| `error`, `pcall`, `xpcall` | Implemented as base functions. Six fixtures match Lua 5.4.9, with `error(v, 0)` wherever a message is compared |
| `local x <close>`, `__close` | Implemented. Four fixtures match Lua 5.4.9; coroutine closes are checked with hand-built bytecode and through the coroutine library |
| Generic `for` | Implemented. Six fixtures match Lua 5.4.9; an iterator that yields inside a coroutine is checked with hand-built bytecode |
| `function (a, ...)`, `...`, vararg chunks, `select` | Implemented. Two fixtures match Lua 5.4.9; a yield and a yielding close with extras live are checked with hand-built bytecode |
| Proper tail calls, `return f(...)` | Implemented. Three fixtures match Lua 5.4.9, one of them 100,000 hops of each kind of tail recursion; a coroutine failing after a tail call is checked with hand-built bytecode |
| `and`, `or`, `not` | Implemented as short-circuit jumps. A fixture matches Lua 5.4.9 |
| Method calls (`:`), `f{...}` and `f"..."` call arguments, `function` statements with dotted and `:` names, `local function` | Implemented as compiler lowering. Two fixtures match Lua 5.4.9, one of them 100,000 hops of method and `local function` tail recursion |
| `goto`, labels | Implemented in the compiler. Two fixtures match Lua 5.4.9, with 47 hand-written legal and illegal programs and 9,000 generated ones |
| `local x <const>` | Implemented in the compiler. A fixture matches Lua 5.4.9 |
| Base library: `assert`, `collectgarbage`, `ipairs`, `load`, `next`, `pairs`, `print`, `tonumber`, `tostring`, `type`, `_G`, `_VERSION` | Implemented (ADR 0031). Six fixtures give Lua 5.4.9's output, and 32,000 `tonumber` conversions match it. See [Base library](#base-library) |
| `math` | Implemented (ADR 0032). A fixture and a corpus of 3,063 calls give Lua 5.4.9's output; `math.random` gives Lua 5.4.9's sequences from explicit seeds. See [Math](#math) |
| `table` | Implemented (ADR 0033). A fixture and a corpus of 568 cases give Lua 5.4.9's output, `table.sort`'s comparison counts included. See [Table](#table) |
| `string`, and the metatable all strings share | Implemented (ADR 0034–0038). Five fixtures and four generated corpora (7,890 lines) give Lua 5.4.9's output. See [String](#string) |
| The registry, `package`, `require`, `searchpath` | Implemented with preload, optional filesystem Lua and host-resolver searchers (ADRs 0039, 0060). See [Registry, `package`, and `require`](#registry-package-and-require) |
| `debug`, including `sethook` and `gethook` | Implemented, and installed only on request (ADRs 0040, 0059). Introspection and hook corpora check Lua 5.4.9 semantics. See [Debug library](#debug-library) |
| `coroutine` | Implemented (ADR 0041). A corpus of 122 lines gives Lua 5.4.9's output under every schedule. See [Coroutines](#coroutines) |
| Weak tables, ephemerons, `__gc` finalizers, `warn` | As in Lua 5.4.9, in both collector modes (ADR 0050, ADR 0051). A 70-line corpus matches Lua 5.4.9, warnings included |
| Incremental and generational collection, `collectgarbage` modes and parameters | As in Lua 5.4.9 but for collection timing. Generational is the default, as in Lua. A `collectgarbage` corpus and a generational corpus match Lua 5.4.9 started in either mode |
| `utf8` | Implemented. The six-field Lua 5.4.9 library, with strict and lax decoding. See [UTF-8](#utf-8) |
| `dofile`, `loadfile` | Provided through optional filesystem/stdin capabilities |
| `os` | Provided behind optional host capabilities; C locale only, UTC fallback without civil authority |
| `io` | Implemented behind optional filesystem/stdio/process capabilities; logical no/full/line buffering. See [IO and OS](#io-and-os-phase-337) |

`docs/LUA_LANGUAGE_AUDIT.md` classifies every production of the Lua 5.4 grammar, lists the compiler's limits against PUC Lua's, and has the official-suite baseline.

Flat left-associative operators and suffix chains (`a + b + c`, `t.a.b`, `f()()`, `o:m():m()`) lower iteratively, as in PUC Lua, and have no expression-depth limit of their own. Genuinely nested expression trees still have a Moonseed 300-edge safety limit. The parser applies PUC-style C-level accounting to recursive syntax.

A table constructor stores its fields in source order. PUC Lua stores list fields in batches after the keyed ones, so `{1, 2, [2] = 9}` has `t[2] == 9` in Moonseed and `2` in PUC Lua. The manual leaves the order undefined; it matters only when two fields have the same key.

`3..4` is one malformed number, as in Lua 5.4.9. `3. .. 4` is a float, `..`, and an integer. Direct compilation and `load` do not strip a shebang; `loadfile`/`dofile` strip an initial `#` line and optional UTF-8 BOM. `\u{...}` follows Lua's range below `2^31`, not Rust's `char`.

A free name is `_ENV.name`. The main chunk's `_ENV` is the runtime's globals table. The main chunk is vararg, as in Lua: `Runtime::load_chunk_with_args` gives it arguments, and `load_chunk` none.

`if` tests Lua truth: `nil` and `false` are false, and every other value, including `0` and `""`, is true. No metamethod is involved. A call condition uses its first result.

Leaving a block closes the block's captured locals, so a closure keeps its value when the register is reused. This is ordinary upvalue closing. `<close>` locals are described [below](#to-be-closed-variables).

## Tables and indexing

`{ a, b; k = v, [e] = w, }` accepts `,` and `;` and a trailing separator. List fields are numbered from 1, whatever keyed fields sit between them. A last list field that is a call keeps all its results, and a nil result still uses up its position. A call anywhere else, or in parentheses, gives one value.

Lua does not specify the order in which a constructor's fields are assigned, which shows only with repeated keys or side effects. Moonseed evaluates and stores fields in source order, one at a time. Tests against Lua use only constructors whose result does not depend on that order.

`t[k]` and `t.name` read a table; a nil or NaN key reads nil, and a missing key reads nil unless `__index` says otherwise. Assigning nil deletes. Assigning with a nil or NaN key faults (`NilKey`, `NanKey`) unless `__newindex` takes it. Indexing or assigning into a value that is not a table goes through its type's metatable (ADR 0034): a string reads the `string` library through the string metatable's `__index`. A value whose type has no metatable, or whose metatable has no `__index` (`__newindex`), faults with `LuaFault::Index`.

In a multiple assignment the tables and keys of all destinations are evaluated first, left to right, then the values, then the stores, right to left. `i, t[i] = 2, 99` writes `t[1]`. Lua guarantees only that the destinations are evaluated before the assignment; the store order is what PUC does.

A float key with an integer value is that integer key (`t[2.0]` is `t[2]`). The float 2^63 is not an integer key.

## Metatables

Tables have metatables. `setmetatable` accepts a table or nil and refuses to change a metatable with a `__metatable` field. `getmetatable` returns that field when present. `rawget`, `rawset`, and `rawlen` bypass metamethods. Invalid arguments to these functions use Lua's numbered argument errors and call-site function names.

- `__index`: consulted only when the raw value is nil. A function is called with the table and key, and its first result is the value. Any other value is indexed again.
- `__newindex`: consulted only when the key is not live. A function is called with the table, key, and value, and the raw store does not happen. Any other value is assigned into.
- A chain of non-function values stops after 2000 steps with `LuaFault::MetaChain`, as Lua does.
- `#`: a string's byte length. A table uses its `__len` (first result), else its raw border. `__len` receives its operand twice, as in Lua 5.4; before bytecode revision 7 it received it once.
- Operators: see [Operators](#operators).
- `__call`: see [Calls](#calls).
- Metamethods are looked up raw, never through `__index`.
- A metamethod may be a Lua function, a native, or any value with `__call` (except for `__index` and `__newindex`, whose non-function values are indexed or assigned into, as in Lua). It may wait on the host and may be checkpointed partway; the instruction that called it finishes once.

`__tostring`, `__name`, and `__pairs` are used by the base library.

A table and a full userdata have their own metatables. Every other type has at most one, shared by all its values, light userdata included (ADR 0034, ADR 0042). The string library installs the string metatable; `debug.setmetatable` sets any other, and `setmetatable` takes tables only, as Lua's does. `getmetatable("")` returns the string metatable, and changes to it apply to every string.

`__mode` and `__gc` are in [Weak tables and finalizers](#weak-tables-and-finalizers).

## Userdata

Host userdata are supplied by the embedder (ADR 0042 to ADR 0045); the IO
library also creates internal file userdata. Once made, userdata behave as in
Lua 5.4.9, checked by a 156-line corpus against Lua 5.4.9 driven through its
C API (`tools/lua54_userdata_harness.c`):
- `type` calls full and light userdata `userdata`; an argument error calls a light one `light userdata`, or names either by its metatable's string `__name`.
- **Full userdata** are distinct objects with their own metatable, a fixed number of user values, and bytes Lua cannot read. Every metamethod event works on them, through the same lookup as tables: indexing, arithmetic, bitwise, comparison, `__eq` (only between two distinct full userdata), `__concat`, `__len`, `__call`, `__tostring`, `__name`, `__pairs`, and `__close`, with yields, host waits, and checkpoints inside metamethods as for tables.
- **Light userdata** are identity tokens: equal only to the same token, never calling `__eq`, sharing one metatable for every other event.
- Both are table keys, compared by identity.
- `debug.getuservalue`, `debug.setuservalue`, and `debug.upvalueid` follow Lua 5.4.9, results, arity, and messages.

What differs:
- **Text.** `tostring` and `%p` show a deterministic identity, not an address: a full userdata's object id, a host key's 16 hex digits, a VM token's id (ADR 0043).
- **Light userdata from the host** are 64-bit keys the host chooses, not pointers (ADR 0043).
- **Limits.** A byte payload is bounded by the heap quota (at most 1 GiB) and a userdata has at most 65,534 user values; userdata count against the heap quota (ADR 0042, ADR 0052).
- **`debug.upvalueid` of a builtin** fails, as Lua's does for a C function without upvalues. Moonseed's builtins have none; Lua's C closures with upvalues correspond to Moonseed's native closures, whose values `upvalueid` names. A `string.gmatch` iterator keeps two values where Lua's keeps three C upvalues, so `upvalueid(iterator, 3)` fails in Moonseed.
- **Finalizers.** A full userdata's `__gc` runs as a table's does (see [Weak tables and finalizers](#weak-tables-and-finalizers)); a host value's Rust `Drop` runs later, when a collection frees the object, and is not `__gc` (ADR 0044).

## Operators

Arithmetic (`+ - * / // % ^`, unary `-`) follows Lua 5.4:
- Integers wrap, and `//` and `%` round toward negative infinity.
- `/` and `^` give floats. `x ^ 2` is `x * x`, as in PUC's `luai_numpow`.
- Integer `//` or `%` by zero faults with `DivideByZero` before any metamethod.
- A string that reads as a number takes part through the string metatable's arithmetic metamethods, as in Lua 5.4: `"40" + 2` is 42, `" 0x10 " * 1` is 16, `-"2"` is -2. Without the string library, or with its `__add` removed, `"40" + 2` is an error; a replaced `__add` is called for every string (ADR 0034).

Bitwise operators (`& | ~ << >>`, unary `~`) take integers and floats with an exact integer value, and not strings, as in Lua 5.4.
- A non-integral float faults with `NoInteger`; any other operand faults with `Bitwise`.
- A shift of 64 or more either way gives 0, and a negative count shifts the other way.

`..` joins strings and numbers. Moonseed writes a number as PUC Lua does: `%d` for integers, `%.14g` for floats, with `.0` added when the text would read as an integer, and `inf` / `-inf`. Lua does not specify this format. Moonseed's digits do not depend on locale or target. Every NaN is `nan`; PUC on glibc writes `-nan` for a NaN whose sign bit is set, which depends on how the target produced it.

When the primitive rule does not apply, the first operand's metamethod is used, else the second's, called with both operands in order. `__unm`, `__bnot`, and `__len` get their operand twice, as in Lua 5.4. The first result is kept. With no metamethod the operator faults: `Arith`, `Bitwise`, `NoInteger`, or `Concat`; the cold diagnostic path names the failing operand as Lua 5.4.9 does.

`a .. b .. c` is right-associative and handled two operands at a time. PUC Lua joins a run of strings in one step, but the order in which `__concat` is called is the same.

Subtraction is IEEE subtraction: `-0.0 - 0` is `-0.0`. PUC Lua compiles `x - k` for a small integer constant as `x + (-k)`, so it prints `0.0` for that expression but `-0.0` for `-0.0 - z` with `z = 0`. Moonseed gives `-0.0` for both.

`^` uses the portable `libm` crate's `pow` on every target, so its results have the same bits native and on wasm32 (ADR 0032). They may differ from the host C library's `pow`, which PUC Lua uses, by 1 ulp.

## Calls

A thread holds at most 1,000 Lua and protected calls. A Lua call past it raises "stack overflow" (`LuaFault::StackOverflow`), which `pcall` catches. Up to 80 more frames are allowed for `xpcall` message handlers running on a stack that overflowed, so a thread holds at most 1,080 frames, the bound a snapshot accepts.

A thread's stack is bounded too, by `Config::max_stack_slots`: 50,000 value slots by default, configurable from 1,024 to 100,000 (ADR 0028). Calls use seven-eighths of it and raise "stack overflow" past that; the rest is kept for message handlers and the closes of a stack overflow's unwind. A `__close` that recurses too deep while another error unwinds overflows with "stack overflow", as in Lua 5.4.9. Deep recursion through functions with many registers, many extra arguments, and very long open result lists reach it before the frame limit. PUC Lua's stack is about a million slots. A snapshot accepts exactly the bound the runtime enforces, so every running state can be checkpointed.
- PUC Lua allows about 200,000 Lua calls, so Moonseed's bound is lower for plain recursion.
- It is higher for metamethods. PUC Lua calls a metamethod from C and stops at about 200 nested C calls ("C stack overflow"). Moonseed runs a metamethod as an ordinary frame, so an `__eq` that compares two tables recurses 1,000 deep before it overflows.

## Logical operators

`a and b` gives `a` when `a` is false or nil, and otherwise `b`; `a or b` gives `a` when it is true, and otherwise `b`. The right operand is evaluated only when needed, and the result is the operand itself, not a boolean: `nil and 42` is nil, `0 or 42` is 0. `not a` is always `true` or `false`. Only `nil` and `false` are false; `0`, `-0.0`, `""`, tables, and functions are true. No metamethod is involved. Each gives one value: `true and f()` keeps only `f`'s first result. Precedence is Lua's: `or` below `and` below the comparisons, `not` with the unary operators.

## `<const>`

`local x <const> = v` declares a local that cannot be assigned after its declaration, as in Lua 5.4. A list may declare any number of them, and each name takes one attribute. An assignment to one, directly, in a list, by a `function` statement, or from a nested function at any depth, is a compile error of kind `Syntax`. The table a const local refers to can still change. Const-ness exists only in the compiler: a const local compiles to exactly the code of an ordinary one, and bytecode and snapshots do not carry it, so hand-built bytecode can write the register. PUC Lua folds some const locals into compile-time constants, which only the debug interface can see; Moonseed does not.

## `goto` and labels

As in Lua 5.4 (ADR 0030):
- A label is visible in the rest of its block and the blocks inside it, not in nested functions. Declaring a label where one of the same name is visible is an error; a label of a finished sibling block may be reused.
- A goto may not jump into the scope of a local, and a goto with no visible label is an error. Both are compile errors of kind `Syntax`, naming the label and, for a scope error, the local.
- A label followed only by labels and `;` to the end of its block counts the block's locals out, so a goto may reach it past them. Before `until` this does not apply.
- A goto that leaves captured locals closes their upvalues, and one that leaves `<close>` locals closes them, newest first, before it jumps; a close that waits, yields, or fails behaves as at any scope exit. Leaving a generic `for` closes its closing value.

## Method calls and function statements

These are compiled into the operations they stand for; the runtime has nothing new for them.
- `v:name(args)` evaluates `v` once, looks `name` up in it with ordinary indexing, so `__index` tables and functions apply and the lookup may wait or fail, and calls the result with `v` first. If the result is not a function, `__call` applies as to any call. `v:name{...}` and `v:name"s"` pass one table or one string, as `f{...}` and `f"s"` do.
- `function a.b.c(...)` is the assignment `a.b.c = function(...)`: a local `a` if one is in scope, otherwise `_ENV.a`; each `.` step is ordinary indexing, and the store is ordinary assignment, so `__newindex` sees it. `function a.b:m(...)` adds `self` before the parameters.
- `local function f` declares `f` before compiling the body, so the body refers to the new local, as Lua's `local f; f = function ... end`. `local f = function` does not.
- `return v:m(...)` and a `local function`'s `return f(...)` are tail calls under the usual rule.

## Tail calls

`return f(args)` is a tail call when the list after `return` is one call, not in parentheses, and no `<close>` local of the function is in scope, a generic `for`'s closing value included, as in Lua 5.4 (ADR 0029). `return (f())`, `return 1, f()`, `return f(), 1`, and `return 2 * f()` are ordinary calls. A tail call to a Lua function replaces the caller's frame: self, mutual, vararg, and `__call` tail recursion run in constant stack space under the 1,000-frame limit. A tail call to a native keeps the Lua frame while the native runs, then its compiled `Return` delivers the results. That frame counts toward the 1,000-frame limit, as a C callee's caller does in PUC Lua; recursion through `return pcall(f)` can therefore overflow. Debug levels and an `xpcall` handler can see the tail-calling Lua frame while the native runs. Older snapshots with an already-erased native tail caller remain restorable.

## Varargs

`function (a, b, ...)` keeps the arguments past `b`, and `...` produces them, as in Lua 5.4 (ADR 0028):
- `...` is legal only directly inside a vararg function. A nested function that is not vararg cannot use its parent's; that is a compile error, "cannot use '...' outside a vararg function".
- `...` follows the rules of a call's results. It is one value in a single-value context and in `(...)`, and its first value in the middle of a list. As the last expression it gives all of them: `return ...`, `f(x, ...)`, `{ ... }`, `local a, b = ...`, and a generic `for`'s four values.
- The count is exact: `f()`, `f(nil)`, and `f(nil, nil)` differ, and interior nils are kept.
- Missing fixed parameters are nil, and extra arguments to a function without `...` are dropped.
- `select(n, ...)` and `select('#', ...)` work as in Lua 5.4, including negative `n`, a numeric string or integral float for `n`, and errors for `0` and for a negative `n` before the first argument.

A vararg function's extras stay below its registers until it returns, so calls, metamethods, protected calls, closes, yields, and waits in between cannot change them.

Calling a value that is not a function looks up its `__call` and calls that, with the value inserted as the first argument, as Lua does. A `__call` value may itself be a table with `__call`. The call's results follow the original call's result count, so `__call` can return several values. Moonseed follows at most 200 `__call` steps for one call, then faults with `LuaFault::CallChain`. PUC Lua is bounded only by its stack, about a million slots, and a cycle takes quadratic time to reach it.

## Globals and `_ENV`

A name that is not a local or upvalue is `_ENV.name`. `_ENV` is an ordinary name: a local or parameter called `_ENV` shadows it, and a closure captures the `_ENV` in scope where it is defined. `local _ENV = { x = x }` reads `x` from the outer `_ENV`. The chunk's own `_ENV` is the runtime's globals table. `Runtime::install_base` adds `_G`, the globals table itself; assigning `_G` changes only that entry, never the `_ENV` of loaded code.

A global can hold any Lua value, including Lua functions and native functions bound by the host.

## Native functions

A native function compares equal to itself and to any other value for the same registered symbol, and unequal to everything else, as a C function does in Lua. It can be a table key. Calling it with `f(...)` follows the ordinary argument and result rules. Calling a value that is not a function and has no `__call` is `LuaFault::BadCall`. A Rust native can call Lua through a VM-owned continuation; see the [embedding guide](EMBEDDING.md#native-to-lua-continuations). `rawequal(a, b)` compares without `__eq`; it needs both arguments.

## Comparisons

`==` and `~=` compare numbers by value across integers and floats, strings by bytes, nil and booleans by value, and tables, functions, and threads by identity. Two distinct tables then try the first's `__eq`, then the second's; they need not share one, as in Lua 5.4. The result is the truth of its first result. With no `__eq` they are unequal. `~=` is the negation.

`<`, `<=`, `>`, `>=` compare two numbers or two strings directly. Mixed integer/float comparisons are exact, including above 2^53 and at the `i64` limits. NaN is unequal to itself and unordered. Anything else tries `__lt` or `__le` on the first operand, then the second, and faults with `LuaFault::Compare` without one. `>` and `>=` swap their operands, as in Lua. `<=` does not fall back to `__lt`: Lua 5.4's manual removed that, although a stock build with `LUA_COMPAT_5_3` still has it.

Strings order by unsigned bytes, a shorter prefix first. PUC Lua uses `strcoll`, so its order follows the process locale. It matches Moonseed only in the `C` locale, which is what the standalone `lua` uses unless a script calls `os.setlocale`. Moonseed does not read the locale. This is a deliberate profile difference.

## Loops and blocks

A `while` or `repeat` body is a new scope on every iteration. A closure that captures a body local gets that iteration's cell.

The locals of a `repeat` body are in scope in its `until` condition. They close after the condition is tested, on both the loop-again and the exit edge.

`break` leaves the innermost `while` or `repeat` in the same function and closes every captured local declared inside the loop. `break` outside a loop is a `Syntax` error. Statements after `break` in the same block are allowed, as in Lua 5.4.

## Numeric `for`

`for v = init, limit [, step]` evaluates the three expressions once, left to right, before `v` exists. The default step is the integer 1.

It is an integer loop when `init` and `step` are integers, whatever `limit` is. The limit is then floored (positive step) or ceiled (negative step). A limit past the integer range clamps to `math.maxinteger` or `math.mininteger`, or skips the loop when it lies behind the start. The loop runs a precomputed count, so it never wraps. `math.mininteger` works as a step. Otherwise all three are converted to floats, and `v` is a float.

A string that reads as a number is accepted, as in Lua 5.4: `for i = "1", "3"` is a float loop over 1.0, 2.0, 3.0, and `for i = 1, " 0x10 "` is an integer loop to 16. The conversion follows Lua's `tonumber` rules and was checked against it string by string.

A zero step, `-0.0` included, is `LuaFault::ForZeroStep`. A value that is not a number is `LuaFault::ForValue`. Lua raises "'for' step is zero" and "bad 'for' ... (number expected)" in the same cases.

Assigning to `v` in the body lasts until the next iteration, which sets `v` from the loop's own index. A closure that captures `v`, or a body local, gets that iteration's variable.

PUC behaviour kept as is: an integer loop with a NaN limit runs zero times for a positive step and runs toward `math.mininteger` for a negative one. A float loop with a NaN limit runs once. A float loop whose step is lost to rounding never ends: starting at -2^63 with step 1, the index stays at -2^63, so any limit at or above it is never passed. Fuel bounds it.

## Generic `for`

`for n1, ..., nk in explist do body end` follows Lua 5.4 (ADR 0027):
- The expression list is evaluated once, left to right, before the loop variables exist, and adjusted to exactly four values as a `local` list would be: the iterator, the state, the initial control value, and the closing value. Missing values are nil, extra ones are evaluated and dropped, and a final call supplies as many as are missing.
- Each iteration calls `iterator(state, control)` with exactly those two arguments. Any callable works: a Lua function, a native, or a table with `__call`, which gets the table first, as any call does. The results are adjusted to `k` values.
- Only a nil first result ends the loop. False is a value: the body runs with it, and it is the next control.
- The loop variables are new locals in each iteration, so closures capture each iteration's values. Assigning one does not change what the next call receives.
- The fourth value is a to-be-closed value for the whole loop, not one per iteration. Nil and false are ignored. Any other value must have `__close` before the first call, or the loop raises "variable got a non-closable value" without calling the iterator (PUC names the variable `(for state)`). It closes once, after the body's own `<close>` locals, when the loop ends by exhaustion, `break`, `return`, or an error, and gets the error in the last case. Everything in [To-be-closed variables](#to-be-closed-variables) applies.
- An iterator may pause, wait on the host, raise, or yield inside a coroutine. Each call runs exactly once, whatever the checkpoints.

`pairs`, `ipairs`, and `next` are base functions (ADR 0031), and generic `for` runs them as any iterator.

## Unary minus

`-x` negates integers with wrap-around (`-math.mininteger` is `math.mininteger`) and floats, else calls `__unm` with `x, x`, else faults with `LuaFault::Arith`. A numeric string (`-"2"` is -2) goes through the string metatable's `__unm`.

## `next`

Lua does not specify enumeration order. Moonseed's order is insertion order: `next(t, nil)` is the first live entry, and later calls follow that order, skipping deleted slots.

The tests against Lua check a contract that does not depend on order: every remaining live key appears once, the deleted current key is not returned, and the walk ends at nil. They also check that `next` of an absent key errors. They do not compare Moonseed's sequence to PUC's sequence.

Supported while iterating: replacing a live value, and deleting the current key or another existing key. Inserting a key that was not live is not a promise. Moonseed's own rule, recorded in ADR 0010, drops dead anchors and appends the new key. Do not treat that as Lua behavior.

The base function `next` walks the same order as the `Next` instruction. It returns the next key and value, or a single nil at the end, as Lua's does. A float key with an integer value is the integer key, here as everywhere: `next({10}, 1.0)` is `nil`, the end. PUC Lua looks such a key up unconverted and raises "invalid key to 'next'".

## Base library

What differs from Lua 5.4.9 (ADR 0031):
- **Error messages** use Lua's numbered argument errors, call-site function names and source positions when called from Lua.
- **`tostring` of a table, function, or thread** shows its kind (or a string `__name`) and its `ObjectId` in hex, not a memory address: `table: 0x0000002a`. A native function shows its registry symbol: `function: builtin: base.print`. The same object gives the same text on every target and after a restore. The exact text is not a promise.
- **`print`** writes to the output the host sets with `Runtime::set_output`; with none, it writes nothing. Output order is Lua's: arguments are written before a later argument's `__tostring` runs, so `print(1, bad)` writes `1` before `bad`'s error. Each write is an external effect committed through the journal: a run restored with the journal it wrote does not write again.
- **Yields.** A coroutine may yield across `pairs` calling `__pairs`, and not across `tostring` or `print` calling `__tostring`, `ipairs`'s iterator calling `__index`, or a `load` reader. These are Lua 5.4.9's answers. Pauses, host waits, and checkpoints are allowed across all of them.
- **`load`:**
  - It loads text, and binary chunks `string.dump` wrote (ADR 0036). A PUC Lua chunk, or a damaged one, is refused with "binary string: bad binary format (...)".
  - Text compile errors use Lua 5.4.9's `chunkname:line: message near token` rendering. Binary-chunk refusal retains Moonseed's format message.
  - A reader may give at most 16 MiB, `load`'s source limit; past that, `load` returns `nil, "source exceeds 16777216 bytes"`.
  - A reader is called until the end of the chunk before anything is compiled. Lua parses as it reads, and stops calling the reader at the first syntax error. The mode is checked on the first piece, as in Lua.
  - Errors from the reader go through an enclosing `xpcall`'s message handler before `load` returns them, as in Lua.
  - The source read so far counts against the heap quota.
  - Lua's `load`, like Moonseed's, does not skip a `#` first line.
- **`collectgarbage`** (ADR 0050, ADR 0051):
  - `incremental`, `generational`, `setpause`, `setstepmul`, `stop`, `restart`, `isrunning`, and `step` with and without a size, with Lua's results: parameters stored as `lua_gc` stores them (divided by four in a byte, the minor multiplier as a byte), a 0 left alone, the previous mode returned by a mode change, `step` true when it ends a cycle.
  - In generational mode `step` follows Lua's `genstep` by state. Normally it is a whole young collection, or, with a size that leaves debt to pay, a whole major one when due, and returns false even after a bad major; `step(0)` clears the debt, so is a young collection. While falling back after a bad major it is a whole cycle (Lua's `stepgenfull`) that returns true while it stays incremental and false once it returns to young collections. A corpus section checks each state against Lua 5.4.9. Moonseed's automatic major collections are incremental cycles in bounded steps, where Lua's stop the world.
  - `collectgarbage("count")` is the exact logical heap: every object's logical size, garbage not yet freed included, as Lua's counts its allocated bytes. The figures are logical costs, not Lua's bytes.
  - `collect` and `step` return after the finalizers their work queued; inside a finalizer every option returns nil, as in Lua.
  - `count` is the logical heap (ADR 0021), not allocator memory.
  - Moonseed starts in generational mode, as the standalone `lua` does; `Config::gc_mode` lets an embedding start incremental.
- **`warn`** checks its arguments as Lua does and sends one warning to the sink the host sets with `Runtime::set_warnings`; control messages such as `@on` go to the sink, which decides what they mean (ADR 0049).
- **`tonumber`** without a base uses the same conversion as arithmetic, which rejects `inf` and `nan`. With a base, it reads as Lua's `l_str2int` does, wrapping on overflow.

## Math

What differs from Lua 5.4.9 (ADR 0032):
- **Float results** come from the portable `libm`, not the host's C library. They may differ from PUC Lua's by 1 ulp: in this machine's corpus, 1–2% of `sin`, `cos`, `tan`, and `exp` results, and 8% of `atan` pairs. The printed `%.14g` text differs only where the difference crosses a rounding edge.
- **The default seed.** PUC Lua seeds `math.random` from the time and an address when the library opens, and when `math.randomseed()` gets no argument. Moonseed seeds from `Config::entropy` through a deterministic stream, or from the host's entropy (`Runtime::set_entropy`). Explicit seeds give Lua's sequences.
- **Error messages** use Lua's argument wording and the Lua call-site position and name; a call through a local or field can therefore name that local or field.
- **`math.min` and `math.max`** compare with `<`, as Lua does, so they take any values `<` takes, strings included, and call `__lt`. No coroutine may yield across that call, as in Lua 5.4.9.

## Table

What differs from Lua 5.4.9 (ADR 0033):
- **Table-likeness.** A value that is not a table passes when its metatable, its type's (ADR 0034), has the needed fields, as in Lua. The string metatable has `__index` only, so a string passes nowhere, unless a program adds `__newindex` or `__len` to it.
- **`table.sort`** after a badly unbalanced partition picks a pivot from the range's bounds, where PUC Lua uses the clock. The order of equal elements is unspecified in both. Until such a partition, the comparisons are PUC's, in PUC's order.
- **`table.pack`** fills its table `1..n`, then `n`: `pairs` walks it in that order.
- **Lists with holes** may have another border than in PUC Lua, as `#` may; both are legal.
- **Fuel.** A table function costs a unit per step of at most 32 reads, writes, lengths, or comparisons, after its first step.
- **Yields.** No coroutine may yield across a table function's metamethods or order function, as in Lua 5.4.9. Pauses, host waits, and checkpoints are allowed, and a checkpoint resumes after the last committed operation.
- **A chain of library functions calling each other**, such as `__newindex = table.insert`, ends at the frame limit with "stack overflow", where Lua 5.4.9 says "C stack overflow".
- **Error messages** use Lua's words for argument and operation errors, including "invalid value (at index 1) in table for 'concat'", "too many results to unpack", and "invalid order function for sorting". Lua call-site positions and names are included where Lua includes them.

## String

What differs from Lua 5.4.9 (ADR 0034–0038):
- **Error messages** use Lua's words, including `bad argument #2 to 'rep' (number expected, got no value)`, `malformed pattern (missing ']')`, and `invalid conversion '%y' to 'format'`. Argument errors use the call-site name (`rep`, `format`, a local, or a field) and Lua caller position; the fallback searches loaded library names.
- **Limits.** A result is at most `Config::max_string_bytes` (by default 1 GiB, so in practice the heap quota bounds it) and must fit under the quota; both are checked before the bytes are made (ADR 0052). `string.rep` past the string limit raises "resulting string too large", as Lua does past its own limit, and past the quota "not enough memory"; `gsub`, `format`, and `pack` past either raise "not enough memory". `string.byte` and `string.unpack` check the stack for their results first; past it they raise "stack overflow (string slice too long)" and "stack overflow (too many results)", as Lua does past its own stack.
- **Locale.** Character classes and `upper`/`lower` use the C locale, for every byte, which is what the standalone `lua` uses unless a script calls `os.setlocale`. Moonseed's `os.setlocale` supports only the C locale.
- **`string.format`:**
  - Output is glibc's for every conversion, in the C locale, from Moonseed's own formatter, the same on every target.
  - Every NaN prints as `nan` (`NAN`), whatever its sign.
  - `%p` prints a token, not an address: what `tostring` shows after the type name (`0x0000002a`), `builtin: <symbol>` for a builtin, a token made from its bytes for a string of at most 40 bytes (so equal short strings match, as interned strings do in Lua), and `(null)` for numbers, booleans, and nil, as in Lua.
  - Moonseed does not promise PUC string-pointer identity for equal
    separately-created strings. Equal long literals in different prototypes
    can have different `%p` tokens (`literals.lua:228`); equality by bytes is
    unchanged. The manual defines object identifiers, not a literal-interning
    policy. Sharing canonical long constants is deferred: it would need changes
    to installer forecasts, GC accounting and snapshot anti-amplification
    checks, rather than a small formatter fix. See [the classified official
    suite](LUA_54_SUITE.md).
- **`string.pack`** uses one ABI on every target: x86-64 Linux's sizes, little-endian, alignment 8 (ADR 0038). On such a host the bytes are PUC Lua's.
- **`string.dump`** writes a Moonseed binary chunk, not PUC Lua bytecode (ADR 0036). Moonseed binary chunks are Moonseed-specific and portable across Moonseed native/Wasm targets; not PUC chunks. `load` reads it back with fresh upvalues: the first is `env` or the globals, the rest nil. The chunk keeps the function's debug information and chunk name; `strip` drops what Lua's drops (ADR 0040).
- **`gmatch`'s iterator** is a function the VM implements (ADR 0035): `tostring` shows `function: 0x...` with its object id.
- **Fuel.** A long string function costs a unit per step of bounded work: 4,096 bytes of a built result, or 256 units of pattern, format, or pack work.
- **Yields.** No coroutine may yield across `%s`'s `__tostring`, a `gsub` replacement function or table, or the string metatable's arithmetic falling back to the other operand's metamethod, as in Lua 5.4.9. A yield through the string metatable's own `__index`, or a `__add` written in Lua, is an ordinary metamethod call and may. Pauses, host waits, and checkpoints are allowed across all of them, and a checkpoint resumes the matcher where it was.

## UTF-8

Lua strings remain byte strings. `utf8` is explicit encoding tooling; it does
not change string indexing, length, patterns or equality. There is no Unicode
normalization, grapheme handling or case folding.

The module has exactly `char`, `charpattern`, `len`, `codepoint`, `offset` and
`codes`. It is in the standard library profile, can be omitted from a sandbox,
and has the same table in `_G.utf8`, `package.loaded.utf8` and `require("utf8")`.

- **Strict and lax decoding.** Strict, the default, accepts Unicode scalar
  values: 0 through `0x10ffff`, excluding `0xd800..0xdfff`. A truthy lax argument
  to `len`, `codepoint` or `codes` accepts Lua's extended 1–6-byte UTF-8 through
  `0x7fffffff`, including surrogates. Both modes reject overlong encodings,
  truncation, bad continuation bytes and values above that extended limit.
- **`char(...)`** encodes integer-coercible arguments in `0..0x7fffffff`, including
  surrogates and values above Unicode's maximum; zero arguments return `""`.
  This is the same encoding as the lexer's `\u{...}` escapes.
- **`charpattern`** has the exact bytes `[\0-\x7F\xC2-\xFD][\x80-\xBF]*`, including
  a NUL. It matches candidate byte sequences, not validated scalar values.
- **`len(s [, i [, j [, lax]]])`** counts sequences starting in the inclusive
  byte range, defaulting to `1, -1`. An empty range returns 0; invalid input
  returns `nil` and the first invalid sequence's byte position.
- **`codepoint(s [, i [, j [, lax]]])`** returns one integer per sequence
  starting in the byte range; `i` defaults to 1 and `j` to `i`. An empty range
  returns no values. Invalid input raises `invalid UTF-8 code`, without exposing
  partial results. The result window is checked against the stack limit first.
- **`offset(s, n [, i])`** navigates by continuation bytes without validating
  encodings. The default `i` is 1 for nonnegative `n`, otherwise `#s + 1`.
  Zero finds the start of the sequence containing `i`; other values find the
  nth boundary or return nil. A nonzero move from a continuation byte errors.
- **`codes(s [, lax])`** returns a stable strict or lax iterator function, `s`
  and 0. The iterator returns byte position and code point, or no values at
  exhaustion, and raises `invalid UTF-8 code` on malformed input. A leading
  continuation byte errors when `codes` is called. There is no hidden iterator
  state: repeated calls in the same mode return the same function value.

Positions are 1-based byte positions; negative positions count from the end.
String arguments accept Lua's numeric coercion, not `__tostring`. Bounds and
argument errors follow Lua 5.4.9. Work is fuel-bounded and checkpointable, with
charged construction buffers and partial result slots. Hooks see one call and
return per semantic call, including iterator calls, with no internal-step events.
The frozen corpus matches 508,865/508,865 cases. The Phase 3.36 official suite
compiles 33/33 files and passes 8, up from 6: `pm.lua` and `utf8.lua` now pass.
See `tests/compat/lua54-phase336.json`.

## Coroutines

The `coroutine` library is Lua 5.4.9's (ADR 0041): `create`, `resume`, `yield`, `wrap`, `status`, `running`, `isyieldable`, and `close`, with Lua's results, statuses (`normal` included), messages, and rules for which calls a yield may cross. Values pass between threads exactly, nil holes and counts included. A failed coroutine keeps its stack for `debug` until it is closed. A `wrap` function closes a failed coroutine before it raises, and puts its caller's position before a string error.

What differs:
- **Closing depth.** Closing suspended coroutines uses heap continuations, so a 1,000-coroutine close chain can complete without PUC C-stack exhaustion. It is bounded by the configured heap/object and fuel policies, not the plain-resume depth budget. A peak-depth checkpoint and completion on a 256 KiB Rust stack are covered by `close_chains_past_the_resume_limit_checkpoint_safely`.
- **Resume depth.** A chain of resumes stops at 196 coroutines with "C stack overflow", where Lua 5.4.9 stops a chain of plain resumes. Lua counts C calls between the resumes too, so a chain through `pcall`s or metamethods stops earlier in Lua than in Moonseed.
- **The stack bound.** Too many arguments or results for the receiving thread's stack fail as in Lua ("too many arguments to resume", "too many results to resume"), at Moonseed's bound (`Config::max_stack_slots`), not Lua's million slots.
- **Error positions.** `coroutine.wrap` and `error` each add the position Lua 5.4.9 adds to a string error. A wrapped native tail call retains its Lua caller (ADR 0029). Non-string error values are unchanged.
- **A builtin as the body** (`coroutine.create(print)`) runs from a hidden Lua trampoline frame; debug levels skip it.

## Registry, `package`, and `require`

As in Lua 5.4.9 (ADR 0039):
- `debug.getregistry()` is a table with the main thread at 1, the globals at 2, and `_LOADED` and `_PRELOAD`, the very tables `package.loaded` and `package.preload` are. Each installed library is in `_LOADED`, in any install order: `require "string" == string`.
- `require` follows `ll_require` exactly: `package.loaded` first, then each of `package.searchers` in turn, a string result added to the message, a function result the loader, called with the name and the loader data; its result, or `true`, is stored and returned with the loader data. `package.loaded` and `package.searchers` are read with metamethods, the searchers themselves raw.
- No coroutine may yield across a searcher or a loader, as in Lua; pauses, waits, and checkpoints may.

What differs:
- Searchers are preload, filesystem Lua (when a filesystem capability exists), and the optional host resolver. `package.path` and `package.cpath` default to empty strings and may be configured with `RuntimeBuilder::package_paths`. `package.searchpath` uses journaled readability probes and Lua 5.4.9 template/error-list semantics. `package.loadlib` is absent; no dynamic C modules are supported. A sandbox without filesystem authority reports only the available searchers.
- `load` binds the runtime's globals, so replacing `registry[2]` does not change the environment of new chunks.
- A library that is not installed is not loaded: a sandbox without `string` fails `require "string"`.

`package.searchpath(name, path [, sep [, rep]])` uses Lua 5.4.9's template
substitution, returns the first readable filename, or nil and the attempted-file
message. Failed probes are journaled too. A filesystem searcher uses the same
backend for probes and source reads; a fresh `require` returns filename as
loader data. A truthy cached `package.loaded` hit performs no filesystem calls
and allocates no Lua objects. Configuring `cpath` does not enable a C searcher.

## IO and OS (Phase 3.37)

`Libraries::STANDARD` includes `io` and `os`; functions and authority are
separate. No filesystem, standard streams, clock, environment or process is
supplied by default. Capabilities and profiles are described in
[EMBEDDING.md](EMBEDDING.md#host-capabilities) and [ADR 0060](adr/0060-host-capabilities.md).

The IO table provides `close`, `flush`, `input`, `lines`, `open`, `output`,
`popen`, `read`, `tmpfile`, `type`, `write` and the three standard file handles.
Methods are `read`, `write`, `lines`, `flush`, `seek`, `close` and `setvbuf`.
File userdata share `FILE*` identity and `__index`, `__gc`, `__close` and
`__tostring`; closed-file checks use that identity. Modes r/w/a, update modes
and binary forms follow Lua 5.4.9. Reads support n/a/l/L, their accepted legacy
spellings and integer byte counts, with oracle-checked EOF and result omission.
Writes accept strings and Lua numbers and return the file or a failure tuple.
The VM does no newline translation.

`io.lines(filename, ...)` returns iterator, nil, nil and a closing file.
Exhaustion, generic-for break, explicit close and shutdown share one close
authority. `io.lines()` uses the default input without closing it. Iterator
read failures raise; ordinary reads return their failure tuple. Explicitly
closing a standard stream returns Lua's failure and leaves it open.

Differences and host boundaries:

- `setvbuf` implements no/full/line visibility through charged pending output
  in the file userdata. Flush, close (including GC/scope/shutdown), seek,
  reads after writes, overflowing a filled buffer, whole bulk-write blocks
  and line-mode newlines emit journaled writes. Switching `io.output` retains
  the old handle's buffer. The frozen Linux PUC oracle uses 4096 bytes with
  NULL `setvbuf` storage regardless of requested size (omitted, zero, positive
  or negative); unbuffered mode uses one byte and retains that capacity on
  later mode changes. Other libc buffer sizes are platform details.
- Filesystem failures use `nil, message, code`: native codes are errno-like
  platform values, with platform-specific strerror bytes; VFS uses stable
  synthetic codes. No-authority denials use synthetic permission code 13.
  Native paths/environment names/commands follow C-string NUL truncation at
  the Lua boundary; VFS itself uses opaque byte keys.
- Live VFS files rebind through the preserved backend; live native files and
  pipes refuse checkpoints, including Pending acquisitions. Closed files
  encode without backend authority. Cursors, lookahead, read buffers and pending output are
  snapshot state. File identity text is deterministic, not an OS address.
- `io.popen` and `os.execute` require explicit process authority. Without it,
  popen is denied, `os.execute()` reports false and command execution fails.
  Native shell commands are not confined by a filesystem root.

`loadfile([filename [, mode [, env]]])` uses the same text compiler and private
binary loader as `load`, including explicit nil environment binding. It names
files `@filename`, stdin `=stdin`, and returns nil/message on failure.
`dofile` raises load errors, executes through a yieldable continuation and
returns all results, including nil holes. Omitted filename uses only supplied
stdin. Bounded handle-free reads permit checkpoints without live native files;
source ranges are journaled separately, not an atomic filesystem snapshot.

OS provides `clock`, `date`, `difftime`, `execute`, `exit`, `getenv`, `remove`,
`rename`, `setlocale`, `time` and `tmpname`. CPU clock and wall time are distinct
capability observations; missing clock access raises `time source not available`.
Explicit timestamps and time tables need no clock. Without civil capability,
local conversion uses UTC; leading `!` selects pure UTC conversion. Date uses
the C-locale formatter; time-table normalization and writeback follow PUC order.
Supplied local offset/inverse/zone-name observations are journaled.

Only the C locale is supported. `os.setlocale` returns C for nil, C, POSIX and
empty requests, nil for unsupported names, and never changes global locale.
String classes/case, patterns, numeric parse/format, IO numeral reads and date
formatting all use C semantics. Native temporary names are securely reserved
inside the root, with platform-specific spelling unlike PUC's `/tmp/lua_...`.
`os.exit` is an uncatchable terminal `ExitRequested` host outcome; it never
kills the embedding process. Requested closing runs main-thread close variables
then finalizers, with pauses/waits/checkpoints during shutdown.

Phase 3.37's frozen host corpus matches **1,270/1,270 native + 599/599 VFS = 1,869/1,869**.
Phase 3.38 B1 retains every original record and adds 350 buffering cases:
**1,620/1,620 native + 949/949 VFS = 2,569/2,569**. `files.lua` now passes
its full/line buffering assertions at lines 675 and 689 and reaches line 760
in its process/standalone-executable assertions; the file still does not pass.
The unmodified official suite compiles **33/33**, passes **14/33** (up from 8),
and has 19 LUA_ERROR files. New passes: `bitwise.lua`, `heavy.lua`, `sort.lua`,
`tracegc.lua`, `vararg.lua`, `verybig.lua`. See `tests/compat/lua54-phase337.json`.
Remaining frontiers include missing T.* C-test methods, C-searcher diagnostics,
PUC binary headers, private `_HOOKKEY`, long-string `%p` identity, stackless
C-stack assumptions, coroutine-driver and standalone-CLI behavior, and
platform buffering details. `package.loadlib` and dynamic C modules remain absent.

## Debug library

The `debug` library includes Lua and host hooks (ADRs 0040 and 0059), and no standard installer installs it: a host calls `install_debug`. `getregistry`, `getmetatable`, `setmetatable`, `getinfo`, `getlocal`, `setlocal`, `getupvalue`, `setupvalue`, `upvaluejoin`, `upvalueid`, `getuservalue`, `setuservalue`, and `traceback` take Lua's arguments and give Lua's results and messages, checked by a 180-line corpus against Lua 5.4.9, and the userdata corpus for the last three (see [Userdata](#userdata)).

What differs:
- **Absent:** `debug.debug` and `setcstacklimit`. They are not stubs; calling one is calling nil.
- **Tail-called builtins.** A builtin called by a tail call (`return debug.traceback()`) keeps the tail-calling Lua frame until its return (ADR 0029), so `debug.getinfo(1)` and traceback can show that frame, as Lua does for a C callee.
- **No C level under the main chunk.** Lua's standalone interpreter calls the main chunk from C, so its tracebacks end with `[C]: in ?`, and a long traceback's "(skipping N levels)" counts that level.
- **C levels outside hook transfer windows have no locals.** Lua names a C function's stack slots `(C temporary)`; hooks expose the semantic native argument/result window.
- **`setupvalue` refuses a builtin's values** (it returns nothing): they are the builtin's own state. `getupvalue` reads them, with the empty name, as Lua reads a C closure's.
- **`<const>` locals** are visible to `getlocal`; Lua folds constant ones away.
- **A `__close` an error runs** sees the levels Lua shows: the frames the unwind leaves are hidden, and the `pcall` that caught the error is its caller (ADR 0041).
- **Lines.** Moonseed emits a test for `while true` and `until true`, so their lines are in `activelines`; Lua's are not.
- **The traceback's name search** goes through `package.loaded` raw and in insertion order; when a function is reachable under two names, the one found first may differ from Lua's, whose order is its hash layout's.
- **A numeric `for` whose state `setlocal` made a non-number** raises "'for' loop state is not a number". Lua's behaviour there is undefined.
- A traceback is bounded by the string limit and the quota.
- **`require` of a name with a zero byte** uses the whole name; Lua's C code cuts it at the zero.
- **A stripped function's call through a local** has no name in `getinfo`; Lua still finds where the local came from.
- **`xpcall(f, function(m) return debug.traceback(m, 2) end)`** can still lose the `error` C level under the handler. Native tail calls now retain their Lua caller.

### Debug hooks (Phase 3.35)

`debug.sethook([thread,] hook, mask [, count])` accepts a function or nil.
`debug.sethook()` and `debug.sethook(thread)` clear the selected thread's hook;
without a thread argument the active thread is selected. Mask characters `c`,
`r`, and `l` enable call, return, and line events; other characters are ignored
and the mask ends at its first NUL byte.
A positive count enables count events independently of the mask; zero or a
negative count disables them. Lua integer counts use PUC's signed 32-bit API
conversion. Replacing a hook resets its countdown.
`debug.gethook([thread])` returns the function, mask in `crl` order, and base
count, or a single nil when no hook is installed. A registered host hook returns
`"external hook", mask, count`.

The Lua callback receives `(event, line)`: `"call"`, `"tail call"`, `"return"`,
`"line"`, or `"count"`. Only a line event has a line argument; other events pass
nil. CALL selects tail events too. Calls run after activation entry; a Lua tail
call runs after frame replacement, with no return event for the erased caller.
Native/builtin calls have call and return events but no line or count events;
implementation helper frames add no events. Returns are observed before the
activation leaves, after its return-time closes and before caller result
adjustment. `debug.getinfo(..., 'r')` exposes `ftransfer` (one-based first
transferred slot) and `ntransfer` only during call/return delivery: Lua entry
starts at 1 for nonzero fixed parameters (zero parameters gives 0/0). Return
windows include all raw results and nil holes. Hook inspection also exposes native transfer slots.

A line event precedes a function's first instruction, a changed source line,
or a backward jump, even on the same line. Call precedes the first line event;
count precedes line when both are due. A count interval measures begun Moonseed
bytecode instructions: continuation steps do not recount an instruction, and
native work, GC and hook bookkeeping are excluded. Suppressed Lua hook bodies
still advance instruction counts and the line cursor, as in PUC, but deliver no
nested events. Count, fuel, and source lines are distinct.

Delivery is suppressed while a hook, its descendants, continuations, and error
handlers run, and restored on return or recovery. Changing or clearing a hook
inside it preserves suppression until it finishes. Hook errors are ordinary
Lua errors caught by `pcall`, `xpcall`, `resume`, or `wrap`; a call-hook error
prevents the callee body, and a line-hook error prevents the triggering
instruction. `__gc` finalizers suppress hooks. `__close` calls are ordinary
observable calls when hooks are allowed, including return, error, goto and
coroutine-close paths.

Hook settings are per thread and can be changed on suspended coroutines.
New coroutines follow PUC Lua 5.4.9 inheritance: mask and base count are copied
with a fresh countdown, but the debug wrapper has no child-thread Lua function.
An inherited Lua wrapper therefore returns `nil, mask, count` from gethook and
delivers no Lua callback; it does not root the parent's function. Host hooks
inherit their registered symbol and settings and do deliver callbacks.

Lua hooks are non-yieldable for every event: `coroutine.yield` raises the
C-call-boundary error. Fuel pauses, checkpoints, and registered native host
waits reached from a Lua hook remain legal. The public host-hook API allows
zero-value yields only for line/count in a yieldable coroutine. Resume executes
the interrupted instruction once, without redelivery or recounting; a count
yield also skips a simultaneous line event. Clearing/replacing a suspended
hook retains this continuation; closing the coroutine cancels it. See
[EMBEDDING.md](EMBEDDING.md#host-debug-hooks).

Documented differences:

- Absolute count and count-yield positions follow Moonseed bytecode, not PUC's
  instruction sequence. Compiler optimizations and fuel are preserved.
- Multiline concatenation of locals can have fewer line events because PUC
  emits operand copies that Moonseed does not need. Exact line traces hold for
  the acceptance corpus, not every compiler lowering.
- The private registry table `_HOOKKEY` is not mirrored. Hook authority and
  roots belong to each live thread; registry edits do not replace its hook.
- Host call/return/tail yields raise a catchable C-call-boundary error. The
  pinned PUC C harness crashes for the unsupported call/return yield probes.
- Stripped chunks have nil line arguments and lose debug names. Moonseed's
  dumped bytes remain its private MSC format, not PUC bytecode; stripped hook
  execution and event metadata are checked independently of those bytes.

The final frozen corpus passes 151/151 acceptance cases; 157/193 cases match
byte-for-byte overall, with 36 mismatches in designated oracle-only position
and unsupported-yield cases. The official-suite harness implements only the
hook-related `T.sethook`/`T.resume` subset, not the C API test library. The
Phase 3.35 suite compiles 33/33 files and has 6 PASS, 25 LUA_ERROR and 2
COMPLETED_WITHOUT; `db.lua` now stops at the private `_HOOKKEY` assertion on
line 328. See `tests/compat/lua54-phase335.json`.

## Length

Raw length (`rawlen`, `RawLen`) is not the `#` operator: it never calls `__len`.

A border `b` satisfies `(b == 0 or t[b] ~= nil)` and `(t[b + 1] == nil or b == math.maxinteger)`. A sequence has one border. A holed table may have several, and Lua may return any of them. Moonseed returns the smallest border. Sequences therefore match Lua exactly. Holed tables are checked by the border predicate, not by numeric equality with PUC.

On Lua 5.4.9 the hole `{[1]=10, [3]=30}` printed `1` and `{[2]=20, [3]=30}` printed `0`. Those happen to be Moonseed's answers. The test would still pass if a PUC build printed another legal border.

Zero, negative integers, non-integral floats, strings, and object keys do not extend the positive-integer prefix. `1.0` is the integer key `1`.

## Errors

An error is any Lua value. `error(v)` raises `v` itself, so `pcall(error, t)` returns the same table `t`. Only a string message with a positive `error` level gets a source prefix. The level selects the Lua caller through the same logical stack as `debug.getinfo`; level 0 and non-string values are unchanged. `pcall` and `xpcall` receive the finished error object.

Runtime errors use Lua 5.4.9's `short_src:line:` position for the failing operation and its operand provenance: local, upvalue, global, field, method, or constant where Lua reports one. This covers indexing, calls, arithmetic, bitwise operations, concatenation, length, comparisons, and `for` errors. Argument errors use `bad argument #n to 'name' (detail)` with the call-site name, adjusted method receiver number or `bad self`, and the Lua caller's position. Native tail calls keep their Lua caller while the native runs. `coroutine.wrap` uses the same location rules. The frozen diagnostic corpus after Phase 3.37 matches 3,815/3,815 PUC cases, including the two `package.searchpath` argument cases absent in Phase 3.34.

Text lexer and parser errors use PUC's chunk, line, message, and near-token display. `load` returns `nil` and that rendered string for syntax errors, including parser C-stack exhaustion. Invalid Moonseed binary chunks retain Moonseed's binary-format error. The standalone uncaught-error CLI adds its own text, which need not match PUC's CLI handler.

`string.dump(f, true)` removes debug lines and names. Errors from stripped chunks therefore lose positions or names where PUC does; bytecode constants can still supply a field or global name. No diagnostic sidecar is serialized. The cold formatter bounds output by the configured string limit and heap quota. If formatting cannot allocate, it uses the reserved class string; a memory error uses the reserved `not enough memory` string without attempting allocation.

Moonseed-specific compile ceilings use Moonseed wording: the 300-edge safety bound for genuinely nested expression trees, the compiler's 99-function structural nesting ceiling, and configured source, instruction, constant, and function-count limits. Flat left-associative and suffix chains do not consume that 300-edge bound. PUC-shared parser and 255-upvalue boundaries use PUC wording. See [ADR 0058](adr/0058-diagnostic-authority-and-static-provenance.md).

`pcall(f, ...)` returns `true` and all of `f`'s results, or `false` and the error. `f` may be anything callable, including a table with `__call` and `pcall` itself.

`xpcall(f, h, ...)` does the same, but calls `h` with the error first. The rules for `h`:
- It must be a function, as in Lua 5.4.
- It runs on top of the failing calls, before they are unwound.
- Its first result becomes the error.
- An error inside `h` calls `h` again with the new error. After 20 levels the error becomes "error in error handling". PUC's bound is its C stack, about 200 levels.
- `h` cannot yield ("attempt to yield across a C-call boundary").
- `h` is not called for "not enough memory" or "error in error handling", as in PUC.

A coroutine may yield inside `pcall`; the protected call is still in effect when it resumes.

What `pcall` catches:
- errors raised by Lua metamethods;
- errors raised by natives, including a native metamethod;
- stack overflow;
- memory errors.

Unwinding closes the upvalues of the functions it leaves, so closures keep the values they last saw. `pcall` never catches fuel exhaustion or a VM error, which means corruption or misuse by the host.

An error nothing catches ends the run with `StepOutcome::LuaError(class)`. The host reads the error object with `Runtime::lua_error`. The thread stays failed; the entry thread unwinds first, closing its `<close>` values, as a host `lua_pcall` would.

A coroutine is different, as in Lua 5.4. When an error escapes it, the coroutine is dead but its stack is not unwound: its `<close>` values stay pending until it is closed (`coroutine.close`). `coroutine.resume` returns `false` and the error; a `coroutine.wrap` function closes the coroutine, then raises the error (see [Coroutines](#coroutines)). The hand-bytecode `Resume` instruction raises the error in the resumer without closing.

## To-be-closed variables

`local x <close> = v` works as in Lua 5.4 (ADR 0026):
- One `<close>` per `local` list; two is a syntax error. `<const>` is rejected as unsupported.
- The local is read-only, from its own function or a nested one; the value it refers to can change.
- nil and false are accepted and never closed. Any other value must have a `__close` metamethod when declared, or the declaration raises "variable got a non-closable value" (PUC names the variable).
- The metamethod is looked up again when the value closes, so a replaced `__close` runs the new one, and a removed or non-callable one raises "attempt to call" then. A table with `__call` works.
- Values close newest first, after the scope's captured locals are closed, when the scope is left by falling through, `break`, a loop's next iteration, `return`, or an error. Each gets the value and the error, or nil.
- A `return`'s values are kept while its closes run, even when a close yields or waits.
- A close that raises does not stop the others: each later close gets the newest error, and the last one propagates. An `xpcall` message handler runs before the closes, and again for each close error. These rules match Lua 5.4.9 case for case.
- A close may yield inside a coroutine, during a scope exit, a return, or a protected call's unwind; resuming continues it.
- Closing a coroutine closes its pending values with its error, or nil, and cannot yield; the coroutine's own `pcall`s do not catch those closes' errors.
- A memory error or a stack overflow still closes every value on the way out. On a stack that overflowed, each close runs in a reserve of frames past the depth limit.

## Weak tables and finalizers

As in Lua 5.4.9 (ADR 0046 to ADR 0049), checked by a corpus against Lua 5.4.9 with automatic collection stopped:
- **`__mode`** `k`, `v`, or `kv` in a short string, read at each collection, so a change applies at the next one. Tables, functions with state, threads, and full userdata are cleared from weak tables; strings, numbers, booleans, light userdata, and builtins are not. A weak-key table is an ephemeron table: a value lives through it only if its key lives without it, across any number of tables. `next` goes on past entries a collection removed.
- **`__gc`** on tables and full userdata, for objects whose metatable had `__gc` when it was set; adding `__gc` later registers nothing. Finalizers run newest registration first, with `__gc` looked up when its turn comes, one argument, results dropped; the object and what it reaches survive that collection, and may be kept, or registered again from the finalizer. An object being finalized is already gone from weak values, still a weak key.
- **Inside a finalizer** no yield, no scheduled collection (`collectgarbage` returns nil; an allocation that needs room still collects, as in Lua), and an error becomes the warning `error in __gc (msg)`; the other finalizers still run. A host wait is allowed.
- **Closing.** `Runtime::begin_close` runs every registered finalizer, newest first, as `lua_close` does; registering during it does nothing.

What differs:
- **Timing.** Moonseed runs every finalizer a collection queues before the interrupted code takes another step; Lua runs them at some later allocation, a few at a time. Lua promises no timing, and Moonseed's is deterministic. A collection the host runs between steps queues finalizers for the next step.
- **Dead stack slots.** An explicit `collectgarbage` clears the slots above its call, as Lua's collector ignores them. Elsewhere a value left in a dead register of the running function may live longer, or shorter, than in Lua, whose register allocation differs.
- **Step size.** A step does `stepmul` units of work per KiB allocated, the manual's meaning; Lua's code does 64 times that, so its default step usually finishes a small heap's cycle and Moonseed's does not. Which `step` ends a cycle differs; that one ends it, and its finalizers run before it returns, does not.
- **An object made during a cycle** survives that cycle, and garbage the program dropped while a cycle marked may wait for the next; a full collection (`collectgarbage()`) frees everything unreachable when it starts, as in Lua.
- **Shutdown** finalizers need `Runtime::begin_close`; dropping a runtime runs no Lua.
- **Error wording** inside the warning is Moonseed's for runtime errors (Phase 3.30), such as a `__gc` that cannot be called.

## Memory

Garbage is collected automatically, generationally by default and incrementally on request, as in Lua, on a schedule that is deterministic: the same program collects at the same points under any quantum and across checkpoints, and the collector's work is charged to fuel (ADR 0021, ADR 0050, ADR 0051). A young collection is never due before `Config::gc_min_debt` logical bytes, where Lua's minor multiplier alone decides. Weak tables and finalizers make collections observable; their timing is deterministic, Moonseed's own (see [Weak tables and finalizers](#weak-tables-and-finalizers)). Thread stacks and userdata (their bytes, user values, and what a host type declares) count against the quota. `collectgarbage` runs a full one, and its `count` reports the logical heap, which does show when collections run.

Two limits apply, and both raise the catchable "not enough memory":
- a count of objects (`Config::max_objects`);
- a quota on logical bytes over all objects (`Config::max_logical_heap`, ADR 0025). The default, 64 MiB, is a provisional setting for untrusted scripts, not part of Lua compatibility; trusted hosts raise it.

With automatic collection on, an allocation that does not fit collects once before it fails. PUC Lua raises the same error only when the host allocator fails.

A string made by `..` is bounded by the string limit and the quota, and a longer one is also a memory error, found before anything is copied; the string library's limits are in [String](#string). A snapshot is bounded by `Config::max_snapshot_bytes` (128 MiB by default), which a full default heap fits with room to spare (ADR 0052).

## Not in this tree

The Lua C API, dynamic C modules (`package.loadlib` and C searchers), non-C
locales and an interactive `debug.debug` shell are absent. Long-string `%p`
identity is an accepted implementation boundary; logical write buffering (no, full and line modes) is implemented. See [LUA_54_SUITE.md](LUA_54_SUITE.md) for every suite file and its classified disposition.
`Jump` does not close upvalues by itself; the compiler emits `CloseUpvalues`
before it.
