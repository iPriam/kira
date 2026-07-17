# Loops: one shape below analysis

`for` exists only in the syntax tree. The analyzer rewrites it into a `while`,
so the HIR, the IR, the bytecode compiler, the VM, the LLVM backend, and the
WASM backend have exactly one loop shape between them. That is the whole reason
`for` cost six files instead of fourteen: a `for` loop cannot disagree with a
`while` loop on any backend, because by the time a backend sees one, it *is*
one.

`break` and `continue` could not take the same route — they are control flow,
not sugar — so they are real nodes all the way down. They needed no new opcode:
the bytecode compiler already emitted `Jump`/`JumpIfFalse` for `while`, and the
VM already executed them. The cost landed entirely in the two backends that
name a jump target rather than compute one.

## The desugar, and why the increment is where it is

`for i in a..b { body }` becomes:

```text
var  <cursor> = a          // hidden, mutable: the iteration state
let  <limit>  = b          // hidden: evaluated once, not per iteration
while <cursor> < <limit> {
    let i = <cursor>       // the user's variable: a fresh immutable copy
    <cursor> = <cursor> + 1
    body
}
```

**The increment precedes the body.** Written the obvious way — stepping the
cursor at the end of the body — `continue` jumps over it and the loop spins
forever. Stepping first means every exit from the body (falling off the end,
`continue`, `break`) leaves the cursor already advanced. This is the one thing
to preserve if the desugar is ever rewritten; a test that only checks `for`
without `continue` will not catch it, so
`continue_still_advances_a_for_loop` exists to.

**The cursor and limit are bound to no name.** `FnCtx::declare_hidden` allocates
a slot without touching the scope stack, so no body can read, write, or shadow
them regardless of what it spells its own variables. Sanitizing a name like
`__cursor` would have been a guess about what users won't type;
`a_for_body_may_declare_any_name_it_likes` pins the stronger property.

**`i` is a fresh immutable copy per iteration.** Assigning to it is the same
error assigning to any `let` is, so a body cannot perturb the iteration.

## Half-open, and no range values

`..` is half-open: `for i in 5..5` never runs, and a descending range never runs
at all. There is no `..<` because `..` already means it. A range is not a value
— `..` is not in the binary-operator table, so `let r = 0..4` is rejected at the
parser rather than producing a range object. Both match the language corpus; see
the oracle's `for` headers and its `dot_dot` token.

## Where a jump is named by depth

Two backends do not get to name a block and branch to it:

- **WASM** names a branch target by *how many labels to pop*, so the same
  `break` is a different immediate depending on how deeply it nests. The
  lowering tracks a label count and derives the immediate
  (`Lowering::branch_to`); a hardcoded `br 1` is correct only for a `break`
  written at the top of a loop body and wrong inside an `if`. Only
  statement-level labels count — an expression's labels (short-circuit, checked
  division) open and close within that expression, and no statement can appear
  inside one.
- **LLVM** takes blocks by reference, so its loop stack holds the test and exit
  blocks directly. A `break` terminates its block, which is what makes the
  unreachable tail after it vanish rather than becoming invalid IR.

`a_deeply_nested_jump_finds_its_own_loop` is the case a hand-computed constant
gets wrong, and it runs on both `wasm32` and `wasm64`.

## Analysis guarantees the target exists

`FnCtx::loop_depth` rejects a `break`/`continue` outside a loop (KSEM041 /
KSEM042) and drops the statement rather than emitting it. Every backend
therefore assumes an enclosing loop — and each still reports a typed error
instead of panicking if the frontend ever lets one through, because a compiler
does not get to end its caller's process.
