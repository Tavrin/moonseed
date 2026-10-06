# ADR 0030 — Labels and `goto`: resolved and checked by the compiler, run as ordinary jumps and closes

## Context

Lua 5.4 has `::name::` labels and `goto name`. The rules are about the source: a label is visible in its whole block and the blocks inside it, not in nested functions; a visible label may not be declared again; and a goto may not jump into the scope of a local. A local's scope ends at the last non-void statement of its block, labels and `;` being void, so a label with only labels and `;` after it counts its block's locals out. A goto that leaves locals must close them: captured locals keep their values in their upvalues, and `<close>` values are closed, newest first.

The runtime already has everything a goto does: `Jump`, `CloseUpvalues`, and `CloseScope`, whose `__close` calls can yield, wait, fail, and be checkpointed (ADR 0026). What was missing was the compiler's side.

## Decision

### Labels and gotos are compiler state only

`FnBuild` gains, per function:
- `labels`: the labels visible at this point, each with its target `pc` and its frontier, the number of locals in scope there. A block's labels go when it ends.
- `gotos`: forward gotos waiting for a label, in source order, each with the locals in scope at the goto and a frontier that is lowered to each block's start as the goto leaves the block.
- `edges`: resolved gotos, with the target and the locals the jump leaves.
- `decls`: every local ever declared in the function, kept after its scope ends, with its register and its final captured and `<close>` flags.
- `blocks`: where labels, pending gotos, and locals stood when each enclosing block began.

None of it reaches the prototype. Label names are not in the bytecode, and no snapshot state changes.

### Resolution

- A goto whose label is visible, necessarily declared earlier, resolves at once.
- Otherwise it waits. When its block ends, it leaves the block: its frontier drops to the block's first local.
- A label resolves the waiting gotos of its block (those at or after the block's mark, which includes gotos that left inner blocks) with its name. A goto whose frontier is below the label's would enter the scope of `locals[frontier]`, and is a syntax error naming that local. Visibility is checked first: a label in a nested block is not visible to a goto outside it, so that is "no visible label", not a scope error.
- Comparing counts is enough, because by the time it is made the goto has been lowered to the label's block, and two points of one block with the same count have the same locals.
- A label declared where one of the same name is visible is a syntax error. A label in a finished sibling block is no longer visible, so the name may be reused.
- A goto still waiting at the end of its function is a syntax error.

### Trailing labels

The parser marks a label `last` when only labels and `;` follow it in its block and the block was not ended by `until`, whose condition still sees the block's locals. A `last` label's frontier is its block's first local. This is Lua 5.4.9's behaviour, checked on labels followed by nothing, by `;`, by more labels, and by a statement, and inside `repeat`.

### Lowering

A goto, and a `break`, emit two instruction slots. When the function's code is complete, every local's flags are final, and each edge is filled in:
- the locals it leaves include a `<close>` one: `CloseScope { from }`, then `Jump`;
- else one is captured: `CloseUpvalues { from }`, then `Jump`;
- else: `Jump`, and a second `Jump` that never runs.

`from` is the register of the first local left. Block exits make the same choice through the same function, `exit_op`.

Deciding at the end matters. A closure that captures a local can come after the goto in the source and still run before it, through a backward jump inside the block, so at the goto the flags are not yet final.

A goto with nothing to close costs one `Jump`. A `CloseScope` for a goto is the scope-exit instruction: its `__close` calls are the same resumable calls, it moves on to the `Jump` only after the last, and an error from one unwinds as from any scope exit, so the goto never lands.

### Register bound

Every consecutive-register window now stops at the 250-register limit before an instruction names a register past it: `slot()` and `expr_to()` refuse such a register with a `Limit` error. Before, an argument list whose values were locals could reach register 251. `FnBuild::finish` refuses a prototype past the limit as a backstop, which no compiled program reaches.

### Revisions

No new opcode and no runtime state: bytecode revision 11, snapshot schema 10, tables 3, fuel 1, GC policy 2 are unchanged.

## Alternatives

- **A `Goto` opcode, or a runtime label table.** The runtime would carry source structure it does not need; the existing instructions already do what a goto does, resumably.
- **PUC Lua's close at the label.** PUC Lua puts one `CLOSE` at a label that a goto leaving captured locals reaches, which code falling through the label also runs, and closes on every backward goto that leaves any local. Filling the goto's own slots at the end runs a close only on the jumps that need one.
- **Deciding cleanup at the goto.** A capture after the goto in the source can still be open when it runs. Deciding then would have to close for every local left, or miss some.

## Consequences

- **Language:** `goto` and labels as in Lua 5.4, checked against Lua 5.4.9 by two fixtures, 47 hand-written legal and illegal programs, and 9,000 generated ones. The only generated differences were runaway loops that reach Moonseed's heap limit before its fuel limit, where Lua's run stops at its instruction cap.
- **Runtime:** nothing new. A checkpoint inside a goto's cleanup is a checkpoint inside a scope exit.
- **Validation:** the compiler guarantees a goto's source legality. The bytecode validator still checks every jump structurally, and does not reconstruct labels from bytecode.
- **Cost:** `PERFORMANCE.md`, Phase 3.19.
- **`break` too:** the milestone review found that `break` still chose its close where it was compiled. With `goto`, a capture after a `break` in the source can run before it, through a backward goto inside the loop body, and the `break` then left the upvalue open on a register later code reused. A `break` is now two slots filled at the function's end, like a goto. The review's three reproductions are in `goto_basic`.
