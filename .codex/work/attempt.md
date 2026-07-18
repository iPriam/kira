# attempt / try / handle

Desugared entirely in `kira-semantics`. No IR node, no opcode, no VM dispatch
arm, no LLVM helper, no WASM lowering learns the construct exists — it becomes
the `if`/`else` chain over an enum tag that `match` already builds, and reuses
that module's arm resolution, chain builder, and payload projection verbatim.

The one thing that was not free is the **nested enum payload**, and it cost a
change on every backend. See below.

## The desugar

`attempt { let v = try f(n); return v * 2 } handle { A { P } B { Q } }` becomes:

```text
let <result> = f(n)                 // hidden: evaluated once
let <rtag>   = EnumTag(<result>)
if <rtag> == <tag of `Error`> {
    let <failure> = EnumPayload(<result>)
    let <ftag>    = EnumTag(<failure>)
    if <ftag> == 0 { P } else { Q }  // the handlers, an exhaustive chain
} else {
    let v = EnumPayload(<result>)    // the `Ok` payload, as written
    return v * 2                     // the rest of the body, nested here
}
```

A `try` is an early exit, and the HIR has no early exit that is not a `return`.
So the statements *after* a `try` are exactly the statements that run when it
succeeded, which makes them the `else` branch. Lowering recurses over the body's
statement list rather than looping: each `try` consumes the remainder into its
own success branch.

That shape is also what makes the corpus's `emxProcess` a definite return with
no trailing `return` — `body_definitely_returns` wants both branches of an `if`
to return, and here both do. Getting the nesting wrong shows up immediately as a
missing-return error rather than as a wrong answer.

Two `try`s in one body emit the handler arms twice. The alternative — one shared
chain reached by a flag — needs a jump the HIR cannot express without inventing a
loop, and the reference requires all `try`s of one `attempt` to share a single
failure enum precisely so the arms *can* be repeated.

## Result is structural, not nominal

`try` accepts any enum with an `Ok` variant and an `Error` variant whose payload
is itself an enum. This is not a simplification: the reference's own failing
tests declare a local `enum Outcome { Ok: Int  Error: AppError }` and `try` it,
so a nominal check against a declared `Result` would reject a program it
accepts. **No generics were needed for this feature**, which is why it landed
before them.

## What was refused

`try` is accepted only as the whole initializer of a `let` directly inside an
`attempt` body. The reference's own diagnostic reads "`try` outside `attempt`
**or in an unsupported position**", and its corpus writes exactly one spelling.
Accepting `try` in an arbitrary expression position would mean inventing an
answer for `g(try f(), try h())` — evaluation order, and which failure wins —
that nothing pins. Every other position is `KSEM137`.

An `attempt` with no `try` is `KSEM143`. Its handlers name variants of an enum
nothing chose, so there is nothing to resolve them against, and the reference
does not pin the program.

## The nested enum payload

This is the part that was not a desugar. A `Result`-shaped value carries its
failure enum inside its `Error` variant, so an enum payload had to be allowed to
be another enum — previously `KSEM118` refused it, and a payload could only be a
scalar or a string.

The VM needed nothing: its payload is already an `Option<Value>`, and
`Heap::copy_value` and `free_enum` already recurse.

WASM needed nothing: `load_field` already treats `Type::Enum` as a handle, and
the bump allocator never frees.

The native backend needed a real change. Its enum box carried a one-word payload
plus an `owns_str` flag, which is not enough to say "this word is a nested enum
handle to clone and free recursively". The flag became a payload **kind**, and
the kind now lives in `kira-runtime-abi` as `EnumPayloadKind` — the backend
writes it into every `kira_rt_enum_new` call and the runtime archive interprets
it in `kira_rt_enum_clone` and `kira_rt_enum_free`, and those two are compiled
separately, so one definition has to serve both.

`RUNTIME_ABI_VERSION` went to 2 and the marker function was renamed to
`kira_rt_abi_version_2`. A stale archive would treat the new kind as owning
nothing and silently leak every nested payload rather than corrupting memory —
still exactly the silent backend/archive disagreement the marker exists to turn
into a link error.

A struct or array payload is still refused. A struct payload does not even reach
the payload check: enums are declared before structs, so the name does not
resolve, and it is reported as `KSEM050` instead. Recorded rather than
corrected — the program is rejected either way.

## Diagnostics

The reference numbers these `KSEM133`–`KSEM137`; those codes were already taken
here, so this port allocated the next free block. One code means one thing.

| Code | Meaning |
|---|---|
| `KSEM137` | `try` outside an `attempt`, or in an unsupported position |
| `KSEM138` | `try` on a value that is not `Result`-shaped |
| `KSEM139` | a reachable failure variant with no handler |
| `KSEM140` | a handler naming something that is not a variant of the failure enum |
| `KSEM141` | two `try`s in one `attempt` failing with different enums |
| `KSEM142` | a failure handled by two arms |
| `KSEM143` | an `attempt` whose body contains no `try` |

`KPAR016` reports a missing `handle` after an `attempt` body, and `KPAR017` a
handler arm that does not start with a variant name.

`handle` is **not** a keyword. The reference lexes it as an identifier, so
`let handle = 1` still declares a local named `handle`; the parser recognizes it
contextually, which costs nothing because nothing else may follow an `attempt`
body.
