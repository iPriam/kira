# Closures

A closure adds **no** IR node, **no** opcode, and **no** backend code. Like
classes, the whole feature is a desugar in `kira-semantics` — but where a class
flattens into a struct, a closure **defunctionalizes** into one.

## The desugar

For each distinct function type the program mentions, one synthesized struct:

```text
struct `(Int) -> Void` { tag: Int, <captures of literal #0…>, <#1…>, … }
```

Field 0 is the tag — which closure literal this value is. The remaining fields
are the *concatenation* of every literal's captures, so two literals of one type
never share a slot. Three moving parts:

- **A function type resolves straight to `Type::Struct(repr)`.** No `Type`
  variant was added, so no match on `Type` anywhere in the workspace changed.
  The analyzer keeps a side map from `StructId` to the function type it came
  from, exactly as `classes` keeps one for the struct ids that came from a
  class. The struct's *name* is the type as written — `(Int) -> Void` — which
  can collide with no declared struct, because an identifier holds no
  parentheses. Diagnostics therefore name the type the way the source spelled
  it, for free.
- **A literal lifts to a top-level function** taking the closure value as its
  first parameter, and becomes a `StructNew` of the representation struct. The
  lifted body reads each capture out of that value in a prologue, so a capture
  costs a local read everywhere after.
- **A call through a closure value calls a synthesized dispatcher** — one per
  function type, built once — which branches on the tag and calls the lifted
  function. The value is the dispatcher's first argument, exactly as a method's
  receiver is its function's.

Everything that reaches the IR is a struct, a field read, an `if`, and a call.

**The last literal is the tag chain's unconditional tail**, not one more tested
branch. That is a correctness requirement, not a saving: a tested last branch
leaves a fall-through, the fall-through needs a `return`, and no value of an
arbitrary result type can be conjured that every backend agrees is of that type.
An earlier draft returned `Int(0)` there, which the VM and hybrid ran and the
LLVM verifier rejected — for `() -> String`, and for a program with no closure
literal in it at all. A function type that is called but has *no* literal
anywhere still mints a dispatcher, and that one alone gets a terminator built by
`default_value`, which is well typed for every `Type`.

The representation struct is not surface. `f.tag` is refused (`KSEM136`) rather
than resolved, because the oracle pins no member access on a function value and
letting the ordinary field path run would publish the desugar as language.

## Why not a function pointer

A function pointer needs an indirect call: a table and an `elem` section in
wasm (neither of which the encoder has), a pointer type and a signature in LLVM,
a new opcode and a new `Value` kind in the VM, and a new value crossing the
hybrid seam. That is the full horizontal slice, plus a `RUNTIME_ABI_VERSION`
question, for one feature.

Defunctionalizing costs none of it, and it is sound rather than an optimization:
the set of closure literals in a program is finite and known once analysis
finishes, so a call through a closure value *is* a branch over that set. Two
different closures reaching one call site — which the corpus does exercise —
works by construction, because the branch is on the value's own tag.

## Ordering: what forced what

Two facts about the analyzer shaped the implementation.

**Synthesized function ids are reserved before they are built.** A dispatcher's
id is needed at a call site, long before its body can exist. So synthesized
functions sit after every declared one, ids are handed out from a counter based
at `callables.len()`, and bodies are filled in against reserved slots. The base
is fixed before any signature resolves, which is why `collect_signatures` cannot
mint one.

**A representation struct's field list grows while bodies are analyzed.** Each
literal contributes its captures as it is found, so a literal's `StructNew`
cannot be built complete — the fields it does not own are not known yet. It is
built with the tag alone and *finalized* after every body is analyzed, padding
the other literals' fields with zero values nothing reads. This is why
`StructTable::push_field` exists, and it is used on synthesized structs only: a
declared struct is complete when it is declared, and appending to one would
change a layout its construction sites already agreed on.

## Captures

The enclosing `FnCtx` **moves into** the inner one for the duration of a closure
body and moves back out after. That is what lets a name resolve outward through
any depth of nesting while every frame still reaches its own state through one
`&mut`, and it is what makes a capture thread through *every* frame it crosses
rather than reaching past the closure between.

A capture local is declared in the closure's **outermost** scope. That is the
whole of shadowing: a parameter is declared first and wins; an inner `let` of
the same name is nearer from its declaration onward, and the capture is what the
name means before it. `CkxTrailingCallbackCaptures` in the oracle's corpus reads
`shadowed` free, then declares it, then reads the new one — a whole-body name
scan gets that wrong in both directions, which is why capture resolution is
scope-accurate rather than a free-variable pre-pass.

**A nested literal claims its tag before its body is analyzed.** A literal
written inside another is lifted while the outer body is being analyzed, so a
row appended after the body would let both read the same `impls.len()` and take
the same tag — and the dispatcher would send every call to whichever was
registered second. The lifted function's id is reserved before the body, so the
row can be complete at the point the tag is handed out. Two closures of one
function type, one nested in the other, is the shape that catches it.

## A `var` capture is shared storage

A `var` a closure mentions moves into a **capture cell** at its declaration:
a share-counted box, on every backend. See [cells.md](cells.md) for the
representation, the seven invariants it keeps, and where each is enforced.

Three facts belong here rather than there, because they are about the closure
desugar:

The decision is made from a **syntactic pre-pass** over the function body, in
`closures::captures`, not from capture resolution. Boxing has to happen at the
declaration, and the capture that needs it is discovered later — by which time
every read of the binding has already been lowered. The pre-pass answers by
name, so it over-approximates: one extra box costs an allocation, and one
missing box is a closure and a frame writing different storage.

A cell-typed capture is **trivially copyable**, and it is the only non-scalar
that is. Copying one takes another hold on a box and owns nothing new, which is
what lets a captured `var` travel with no `move` at the site that captures it.

A capture local of cell type is the **only mutable capture**. Every other
capture is a copy, so writing to one would change something nothing can see.

`docs/ownership.md` in the oracle records that closure escape analysis and
capture-by-move are absent there. Here a closure that escapes its frame is
sound rather than unchecked: the box is heap storage the escaping closure holds
a share of, which is what `a_closure_outlives_the_frame_that_declared_its_var`
proves on all three backends.

## What is refused, and why

**A non-trivially-copyable capture is refused by the same code.** The oracle's
`isTriviallyCopyable` admits void, the integers, the floats, and booleans —
never a `String`, a struct, an array, or an enum. Those are the "non-Copy owned
captures" `KSEM117` names, and refusing them is what the oracle does.

**A closure has no `self`.** A closure lifted out of a method cannot read a bare
field name: `self` belongs to the enclosing frame and the lifted function has no
receiver. The body reports an undefined name, which is accurate. No corpus site
writes one.

**`f(1)(2)` is not surface.** Calling the result of a call directly is written
nowhere in the corpus — every site binds the closure first — so it is not
invented here. The parser leaves the second argument list alone, and the type
error that follows names the real shape.

## Diagnostics

Codes are this repo's own, assigned fresh.

| Code | Case |
|---|---|
| KPAR038 | a function type with no `->` |
| KPAR039 | a closure parameter that is not a name |
| KPAR040 | a trailing closure after something that cannot take one |
| KSEM117 | a capture that is not trivially copyable, or a `var` with no shared form |
| KSEM134 | a closure where no function type is expected |
| KSEM135 | a closure whose parameter count does not match its type |
| KSEM136 | member access on a function value (`f.tag`) |

`KSEM062`, `KSEM063`, and `KSEM032` are reused for a closure call's argument
count, its argument types, and its body's result — a closure call is a call, and
giving it its own codes would say the same thing twice.

## Parsing

One bounded lookahead decides everything: a `{` opens a closure exactly when
`in`, or a comma-separated run of identifiers then `in`, follows it. A struct
literal's first field is `name =` or `name :`, and a block's first statement
starts with a keyword, a literal, or a name followed by something else — so
nothing else can look like that.

The oracle carries a second lookahead for a closure header with a mistake in it
(`{ a, b }`, `{ a, 1 in }`), and it is reproduced: a comma is the tell, and
recognizing it is what turns one bad parameter into one diagnostic instead of a
cascade of "expected an expression".

The trailing form is gated on struct literals being permitted, because both
answer the same question — whether a `{` after an expression belongs to the
expression or opens the body of an enclosing `if`/`while`/`for`/`switch`.

## Where it is proven

- `crates/kira-cli/tests/backend_parity/closures.rs` — 13 cases on vm, llvm, and
  hybrid.
- `crates/kira-cli/tests/backend_parity/captured_vars.rs` — 11 more for shared
  mutable captures, each looping its interaction 100–200 times.
- `crates/kira-semantics/src/tests/closures.rs` — the diagnostics.
- `crates/kira-parser/src/tests/closures.rs` — the grammar and its recovery.
- `crates/kira-lsp/tests/protocol.rs` — closures reach the editor through the
  shared frontend, with no LSP change.
- `examples/closures/closures.kira` — parity-checked automatically.
