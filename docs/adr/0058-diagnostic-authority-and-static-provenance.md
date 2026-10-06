# ADR 0058 — Diagnostic authority and static operand provenance

Status: Accepted

Date: 2026-10-04

## Context

Lua 5.4.9 errors name the source position and, for many runtime faults, the local, upvalue, global, field or method that supplied the failing value. Phase 3.34 needed those names without work in successful interpreter dispatch. The owner preferred a sparse compiler-produced diagnostic sidecar. Phase 3.24 already keeps instruction lines, local live ranges, upvalue names and call names in debug information.

## Decision

`runtime/diag.rs` is the cold authority for runtime message text, location prefixes and operand names; `crate::chunkname::chunk_id` supplies short source names to it, the compile renderer and debug introspection. Runtime faults decode the failing instruction and reconstruct temporary-register provenance by scanning prior bytecode writers. `Op::writes` exhaustively declares every register or result window an instruction writes, and `Op::jump_offset` declares explicit branches. The findsetreg-style walk discards a candidate writer if a forward control-flow join or backward loop edge makes it ambiguous. Active locals and upvalues, constant keys and the remaining debug information complete the lookup. Argument errors use the same cold location and call-site naming path. Structured lexer/parser errors have one bounded renderer for host display and `load`.

No operand-name sidecar is stored in a prototype. This decision adds no snapshot, bytecode or binary-chunk format revision and no diagnostic metadata memory charge. `string.dump(f, true)` drops debug information; stripped diagnostics degrade as in PUC Lua, while names derivable from constants can remain. Native tail calls retain their Lua frame while the native runs, as required for argument names and caller positions; [ADR 0029's amendment](0029-proper-tail-calls.md#amendment--2026-10-04-native-tail-calls-retain-the-lua-frame) records that semantic change.

## Alternatives

- **Compiler sidecar:** would make some names direct to read but require a new serialized debug field, snapshot and binary-chunk revisions, and prototype memory accounting. The bytecode scan met the frozen provenance and stripped-chunk corpus instead.
- **Mechanical port of PUC's symbolic walk:** PUC's instruction and control-flow rules do not map directly to Moonseed's opcodes and result windows. An explicit `writes`/`jump_offset` contract covers Moonseed's actual bytecode.

## Consequences

New opcodes must declare their writes and jumps for cold diagnostics. Ambiguous control flow yields no invented name. Error formatting is bounded by the string limit and quota, with reserved-string fallback; memory errors never allocate a diagnostic. Successful dispatch performs no provenance scan. The final pre-FLAT diagnostic corpus matched 3,805/3,807 Lua 5.4.9 cases; the two remaining cases call absent `package.searchpath`. The later FLAT work matched eight additional cases without changing previously accepted unstripped chunks.
