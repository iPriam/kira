# Classes

A class is a struct with a flattened field list and per-class monomorphized
methods. It adds **no** IR node, **no** opcode, **no** backend code — the whole
feature is a desugar in `kira-semantics`.

## What the oracle actually pins

`tests-kik/harness/app/classes/Classes.kira` carries a parity note that decides
the design:

> this module intentionally never lets an *inherited* base method call
> `self.<m>()` where a descendant overrides `<m>`. That self-dispatch is
> virtual on vm/hybrid but static on llvm (a known backend divergence), so it
> is avoided here.

So the oracle **does not pin virtual dispatch** — it has a live backend
divergence there and steers around it. Everything the corpus does exercise is
resolvable statically:

- No corpus site ever binds a derived instance to a base-typed name. The one
  helper that takes a class parameter, `clsChargeBridge(b: ClsBridge, …)`, is
  commented "take a concrete leaf type so dispatch is unambiguous".
- `override let rate = 5` changes the **default of one shared slot**, not a new
  slot: `s.gross()` is 100*5 because `gross` reads the single `rate` field.
- The type may be restated (`override let rate: Int = 5`) and doing so is inert
  — the slot keeps the type its declaration gave it. A restatement that
  *disagrees* is `KSEM059`: an override is chosen by name, so a wrong type there
  means the author believed they were rebinding a different field.
- `ClsAccount.gross()` inside a `ClsSavings` method runs the parent's *body*
  against the *derived* instance — it is spelled "super", not "upcast".

## The design

Monomorphize. For class `C`, register one callable per `(C, A, m)` where `A` is
`C` or an ancestor and `m` is a method written directly in `A`; the body is
`A`'s, the receiver type is `C`. `self` is therefore always statically the
concrete class, so static and virtual dispatch **coincide** — the divergence the
oracle documents cannot arise here, on any backend.

This works because `Callable` already carries `receiver: Option<StructId>`
separately from `function: &Function`. Registering a parent's AST body under the
child's `StructId` is monomorphization for free, with no new machinery.

Fields flatten parent-first, keyed by `(owner, name)` so multiple inheritance
keeps both `ClsAlpha.v` and `ClsBeta.v` as distinct slots. An `override let`
rewrites the inherited slot's default and nothing else.

## What is refused, and why

**No subtyping.** Binding or passing a derived instance where an ancestor type
is written is a typed error. The corpus never does it, so nothing pins what it
would mean, and admitting it is exactly what would resurrect the vm/llvm
dispatch divergence. Refusing keeps every class instance's static type equal to
its dynamic type, which is what makes monomorphization total.

## A class instance is a value, not a reference

No corpus site binds a class instance twice, so the oracle was asked directly.
Given `class Account { var balance: Int = 10 }`, binding `let a =
Account(balance: 1)`, then `var b = a`, then `b.balance = 99`, it prints `1`
then `99`: the second binding copied, and `a` remained usable afterwards, so
the bind was a copy and not a move. A class instance therefore behaves exactly
as a struct instance does.

`a_class_instance_is_a_value_not_a_reference` in the parity suite pins the same
program on vm/llvm/hybrid here.

## Diagnostics

Codes are this repo's own, assigned fresh — they are not the oracle's numbering.

| Code | Case |
|---|---|
| KSEM003 | unknown parent type |
| KSEM004 | class name already defined |
| KSEM062 | wrong constructor argument count |
| KSEM063 | argument type mismatch — the pre-existing call/constructor check, which is also what refuses a derived instance where an ancestor type is written |
| KSEM064 | inheritance cycle |
| KSEM059 | `override let` restates a type the inherited field does not have |
| KSEM065 | duplicate parent type |
| KSEM066 | override signature mismatch |
| KSEM067 | ambiguous inherited method lookup |
| KSEM068 | ambiguous inherited field lookup |
| KSEM069 | invalid parent qualification (qualifier is not a parent, or used outside a method) |
| KSEM072 | `override let` overrides no inherited field |
| KSEM073 | `override function` overrides no inherited method |
| KSEM074 | redeclaring an inherited field without `override` |
| KSEM082 | constructor field has neither value nor default |

A class may extend a `struct`, not only a `class` — `FsaAmbiguousInheritedFieldLookup`
and `FsbInvalidParentQualification` both do.
