# ADR 0011 — Source frontend for one closure fixture

## Context

Frames, calls, upvalues, fuel, collection, and checkpoints already run from hand bytecode. The next step is Lua source bytes compiled into that bytecode. It is not a new VM and it is not the whole Lua grammar.

## Decision

The lexer reads `&[u8]`. Every token and lexical error has a half-open byte span. Line and column are derived from those bytes and are not identity. Identifiers use Lua's Latin alphabet. A leading `#` is the length operator. The `lua` program's shebang strip is not part of `load`, so it is not part of this lexer.

The lexical surface is Lua 5.4: keywords, names, short and long strings, comments, numerals, the operator spellings, and EOF. Long brackets match one level and do not nest. A `[` followed by `=` but no second `[` is an invalid long-string delimiter. The same text after `--` is a short comment, which is what Lua does. Short-string escapes include `\x`, decimal bytes 0 through 255, `\z`, and a backslash followed by a newline. That backslash inserts one `\n`. `\z` skips whitespace and inserts nothing. `\u{...}` accepts a code point below `2^31` and encodes it with Lua's original UTF-8 width, including surrogates and values above Unicode's maximum. It does not use `char::from_u32`.

Numeral text is scanned the way Lua 5.4 scans it: digits, dots, and an exponent marker are consumed, then the text is accepted or rejected as one number. `3..4` is therefore a malformed number. `3. .. 4` is a float, `..`, and an integer. A dot or an exponent makes a float. A decimal integer that does not fit in `i64` becomes a float. A hexadecimal integer with no dot and no `p` wraps to `i64`. Hexadecimal floats are parsed in this crate, not by Rust's decimal parser. The radix point is `.`, with no host locale.

The parser is precedence climbing. The precedence table includes every Lua operator, including right-associative `^` and `..` and unary operators below `^`. Only `+` is compiled. Anything else lexically valid but outside the subset is `Unsupported` or `Syntax`, with a span. It is not miscompiled.

The compiled statements are `local`, assignment to names, `return`, and a call used as a statement. Expressions are nil, booleans, integer and float and string literals, names, parentheses, `function (...) end`, calls, and `+`. Parameters are names, not `...`. A semicolon is an empty statement. `return` ends the block. The new locals in `local a = a` are not visible on the right-hand side. A later local with the same name gets a new register. A name that is not a local or an upvalue would be `_ENV`; it is rejected. There is no globals table.

A nested function that reads a parent local gets `Capture::Local(reg)`. A function that reads a name the parent already holds as an upvalue gets `Capture::Upvalue(index)`, and the parent gains that upvalue if it did not have it. Two closures of the same local both say `Capture::Local` of that register. The existing runtime then shares one cell. Locals, upvalues, constants, child prototypes, and registers are numbered in lexical encounter order. Identical string constants reuse the first index. Hash iteration does not choose any of those numbers.

`compile` returns a detached prototype or an error. The prototype is validated, then `Runtime::boot` installs it. A syntax or compiler error never builds a runtime. Validation rejects bad registers, constants, upvalues, child indexes, call windows, and jumps. The same check rejects a test that builds one broken prototype. There is no bytecode file loader.

Instruction spans stay on `CompiledChunk`. They are not stored in the prototype the VM runs, and they are not in the snapshot. Restore therefore has no source map. That waits for an immutable code image.

`LoadBool` is opcode tag 31. Boolean values were already snapshot fields. No existing field moved, so the schema version stays 3. A snapshot that contains the tag is still schema 3. An older decoder rejects the tag as unknown. Schemas 1 and 2 stay unrestored.

`+` lowers to the existing integer `Add`. A non-integer operand faults at runtime with the existing type error. No float-arithmetic opcode was added. `Jump` still does not close upvalues. This fixture has no jump out of a captured local. That gap is the blocker for `if`, loops, and `goto`.

Frontend limits, not a sandbox profile: 1 MiB of source, 64 KiB for one literal, 1024 bytes for one numeral, 200 parser frames, 64 compile-stack functions including the chunk, 200 locals, 200 upvalues, 250 registers, 256 prototypes, 4096 constants, 10000 instructions in one prototype.

## Alternatives

Copy the PUC lexer. The behavior above was checked against Lua 5.4.9 and reimplemented.

Treat `3..4` as the integer 3, `..`, and 4. Lua 5.4.9 rejects that text as one malformed number.

Bump the snapshot version for `LoadBool`. Nothing already written changed shape, and a snapshot without the new tag still restores.

Compile `_ENV` as a direct globals map. That would freeze a non-Lua global model. Free names stay unsupported.

## Consequences

Control flow from source has to close captured locals on jumps before `if`, loops, or `goto` can be called Lua compatible. A future field cache still misses on the table rules in ADR 0010. This compiler does not add a cache, and it does not put spans or register-allocation scratch into the checkpoint.
