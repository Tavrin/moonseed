# ADR 0037 — Lua patterns as an explicit, resumable machine

## Context

Lua's pattern matcher (`lstrlib.c`'s `match`) is recursive C. It recurses for `?`, `*`, `-`, captures, and each greedy repetition's backtracking, and bounds the recursion at 200 calls (`MAXCCALLS`), raising "pattern too complex" past it. A hostile pattern can backtrack exponentially without calling Lua.

Moonseed must not use the Rust stack for depth Lua code controls, must charge fuel for work, and must be able to checkpoint anywhere, including in the middle of a search.

## Decision

`strpat.rs` is `match` as a state machine:

- **Recursion:** each place C recurses pushes a frame on an explicit stack; each return pops one and resumes its continuation, including undoing a capture when the match under it failed. C's `goto init` loops stay loops.
- **Depth:** the stack counts exactly the calls C counts, so "pattern too complex" is raised where PUC raises it, at 200 frames.
- **Captures:** at most 32, held as byte offsets; strings are made only for results.
- **Classes:** the C locale's, for every byte.
- **Scans:** `find` and `match`'s loop over start positions, `gmatch`'s iterator, `gsub`'s loop, and plain search are machines around it. `gmatch` treats `^` as a literal, as Lua 5.4.9 does.
- **Budget:** each transition costs a unit; a counting loop costs a unit a byte; a back-reference compare or a scan is charged by its length in one go. A step does 256 units (ADR 0034); a debt from a bulk operation carries into the next steps.
- **State:** every machine encodes to a list of integers and decodes with checks that its offsets lie within the subject and the pattern, so a decoded state cannot index out of bounds. A checkpoint anywhere resumes the same matcher, never redoing work.

Two shortcuts keep common searches fast without changing any result, error, or position:

- **First byte.** When the pattern (or the rest of it after a repetition) starts with a plain byte that must be there, positions without that byte are skipped: C's matcher fails at them on that first item without reading on.
- **Counting.** A greedy repetition counts its bytes in one step while the budget lasts.

The engine was built first on its own and compared with Lua 5.4.9 on 80,324 cases (every construct, malformed patterns, the depth bound, 32 and 33 captures), plus adversarial shapes; the same comparison ran again after the shortcuts.

## Alternatives

- **Porting the recursion.** A pattern controls the depth, and a pause could not fall inside a match.
- **A regex crate.** Lua patterns are not regular expressions; back references, `%b`, `%f`, and position captures have no equivalent, and fuel could not bound the work.
- **A bytecode-compiled pattern.** Faster, later perhaps; it would change nothing observable.

## Consequences

- The matcher costs more per transition than PUC's recursive C, four to six times. The shortcuts make a search with a literal start faster than PUC's, and a greedy backtrack before a literal about half PUC's time (`PERFORMANCE.md`). Where no shortcut applies the full cost shows: the milestone review measured `(.-)%1d` over 4,001 bytes at 8.7 s for 20 searches, against 1.5 s in PUC Lua.
- A pathological pattern runs until fuel stops it, a step at a time; the stack never passes 200 frames.
