# `match`: a desugar that needed one expression

`match` is the third construct to land entirely in the analyzer's control flow
— and the first to show where a desugar stops paying. The arms cost nothing:
they become the same `if`/`else` chain `switch` becomes, so no backend learns
`match` exists. But binding a variant's payload was not expressible in the HIR,
and that cost one new node, `HirExpr::EnumPayload`, implemented in all four
backends.

One expression node beat one statement form in every layer. That is the trade
to look for the next time a construct is *almost* a desugar: push the novelty
into the smallest node that carries it, then desugar everything around it.

## The shape

`match e { A -> P  B(x) -> Q }` becomes:

```text
let <subject> = e              // hidden: evaluated once
let <tag>     = EnumTag(<subject>)
if <tag> == 0 { P }
else           { let x = EnumPayload(<subject>); Q }
```

The tag is bound once rather than read per arm, because a chain of N arms would
otherwise clone and drop the enum N times to ask N questions about one value.

## Why the last arm is the `else`

Not an optimization — the truth, and load-bearing. Because coverage is checked,
the last arm runs whenever no earlier one did. Making it the unconditional tail
is also the only way this type-checks:

```kira
function areaOf(c: Circle) -> Int {
    match c { Filled(shape) -> return shape.area; Empty -> return 0; }
}
```

`body_definitely_returns` requires *both* arms of an `if` to return, so a chain
ending in an empty `else` would never satisfy it and this function would be
missing a return. The corpus writes it with no trailing `return`, so the shape
is forced rather than chosen.

## Why `match` is checked and `switch` is not

A `switch` label is an arbitrary expression: there is no set of labels to be
exhaustive over, and no notion of two labels being the same one. A `match` arm
names a variant of a known enum, so both questions have answers, and the corpus
expects them asked — `KSEM129` for a variant no arm covers, `KSEM127` for one
matched twice. Neither check belongs to the other construct, and neither was
added to it.

A resolution failure suppresses the coverage report: an arm naming a variant
that does not exist is one mistake, and letting it also surface as that
variant's absence would tell it twice.

## The payload read is where the backends differ

`EnumPayload` yields an *owned* copy, so the binding outlives the enum it came
from and the box keeps owning its own. That single sentence is what each
backend had to implement differently:

| Backend | How |
|---|---|
| VM | `Heap::enum_payload` deep-copies the payload `Value` |
| LLVM | `kira_rt_enum_payload` clones a `String`, then the word is decoded back to the payload's type |
| wasm | a load at offset 8 in the box, then `copy_if_mutable` |

The native path is the one to watch: the box stores a type-erased word, so the
decode has to be the exact inverse of the encode — a bitcast for `Float`, a
truncation for `Bool`, an int-to-ptr for `String`. A wrong width there is a
silent wrong answer rather than a crash, which is why the parity suite exercises
every payload type the declaration admits rather than just `String`.

## Refused, because the corpus does not pin them

Guards (`A if cond ->`) and multi-pattern arms (`A, B ->`) exist in the
reference's AST and have **zero corpus call sites**, so they are not
implemented. A wildcard arm (`_ ->`) appears nowhere at all — which is what
makes exhaustiveness meaningful, since every arm must name a variant. A payload
binding is immutable for the same reason: the corpus only ever reads one, and a
writable binding would raise a question (does the write reach the enum?) nothing
answers.

## Wire additions

Both append-only, so no `RUNTIME_ABI_VERSION` bump: opcode `ENUM_PAYLOAD =
0x37`, and the `kira_rt_enum_payload` helper.
