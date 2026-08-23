# Trait system, phase 2

Phase 1 (the `Own C storage by the values that create it` and `Release lent call
temporaries newest-first` commits) built the conformance core: derived
`!Copyable`/`Drop` facts on the type table, drop glue in both engines, and the
internal non-denotable `Type::CBlock`. Phase 2 is the surface syntax and the
user-facing conformance machinery on top of it.

## Slice status

- **Slice 1 — trait declarations, conformance, static dispatch.** Landed.
- **Slice 2 — user `Drop`.** Landed.
- **Slice 3 — backed-declaration respelling.** Not started.

## Design decisions taken here

### Receivers are written as a leading `self` parameter

The approved shape spells `function hash(borrow self) -> Int`, and the grammar
had no receiver spelling at all. Rather than fall back to the implicit receiver,
`parse_signature_params` reads a leading `self` — `borrow self` or
`borrow mut self` — into `Function::receiver`, and every other parameter
position refuses one (`KPAR074`). Omitting it still means `borrow self`, so
every method written before this parses unchanged.

A bare `self` is refused (`KPAR075`) rather than read as a borrow: it reads like
a consuming receiver, and the language has none. `borrow mut self` seeds the
mutating-method fixpoint, which is what makes it mean something — a method
declared mutable is mutable whether or not a statement in it writes yet.

### Enums take no conformance list

The approved design lists struct, class, and construct. An enum's variant
grammar already uses `Name: Type`, and a header colon on an enum is a parse
error today; it stays one. Extending conformance to enums is a later question
because it needs a per-variant answer for the `Drop` case.

### One conformance site per (type, trait)

`struct Mesh: Hashable { … }` and `extend Mesh: Hashable { … }` are two
declarations of the same conformance, so writing both is `KSEM290`. The colon
list is the declaration, not a statement of intent to be discharged elsewhere.

### A user `Drop` value moves, and a read of one takes it

The two engines copy differently: a VM struct copy is a share of one object, so
a release runs the body only for the last holder; a native copy is a second
value, and releasing it runs the body again. `TypeTable::runs_user_drop` makes
such a type move on bind, which is what leaves exactly one owner — and a read of
a local holding one *takes* it, on both engines (`Instruction::TakeLocal`, and
the native liveness flag), so a value moved into a callee dies there rather than
a second time at the binding it left.

Two positions are exceptions, because neither consumes: the base of a member
read, and an argument in a borrow parameter. Both compile through a borrowed
read that leaves the local holding its value.

### The VM parks a dropping object; it never calls Kira from the heap

The VM heap is structurally typed, so an object cannot be asked what type it is
at the moment its last holder goes — which is exactly when the body has to run.
So the body travels with the construction (`Instruction::NewStructDropping`),
and the release *parks* the object whole instead of freeing it. The dispatch
loop enters the body as an ordinary frame and releases the object once that
frame is gone, which is what puts the body before everything the value holds
without ever re-entering the heap.

### Native lends a borrowed `Drop` value in every module kind

A copy of a value that runs a body is a second value with the same body to run.
`kira_ir::mid::Lending` gained a `user_drop` field so the two engines can answer
that question differently: native always lends one by pointer, and the VM copies
it the way it copies everything else, because there the copy is a share.

### An impl block on a class goes through the class machinery

`extend <Class> { … }` already joins a class's own methods while it is
flattened, which is what lets a subclass inherit them. `extend <Class>: Trait`
keeps that path and `trait_callables` skips it, so a trait method written for a
class is inherited by its subclasses exactly as a written one is.

## New diagnostic codes

Parser:

| Code | Concern |
| --- | --- |
| `KPAR071` | A conformance list expected a trait name. |
| `KPAR072` | A `trait` declaration expected a name. |
| `KPAR073` | A trait member that is not a `function`. |
| `KPAR074` | A `self` receiver where no declaration can have one. |
| `KPAR075` | A bare `self` receiver. |
| `KPAR076` | An impl block naming more than one trait. |

Semantics:

| Code | Concern |
| --- | --- |
| `KSEM288` | A trait name another declaration already holds. |
| `KSEM289` | A conformance naming something that is not a trait. |
| `KSEM290` | One type conforming to one trait twice. |
| `KSEM291` | A conformance declared outside both owning packages. |
| `KSEM292` | A requirement the conforming type never presents. |
| `KSEM293` | An implementation whose shape disagrees with the requirement. |
| `KSEM294` | An impl block member the trait never declared. |
| `KSEM295` | A trait named in a type position. |
| `KSEM296` | A supertrait clause. |
| `KSEM297` | A `Copyable` claim the type's members refute. |
| `KSEM298` | A conformance on something that cannot conform. |
| `KSEM299` | A `self` receiver on a declaration that is not a method. |
| `KSEM300` | A direct call to a user `drop`. |
| `KSEM301` | A `Drop` conformance whose `drop` member is missing or misshapen. |

## Test counts

`tests-kik/harness` moved from 1274 to 1293 cases, matching on vm and llvm. The
pin lives in `crates/kira-cli/tests/kik_harness.rs`.

## Sibling repositories

Nothing yet. Slice 3 changes the backed-declaration head, and the list of what
siblings must migrate is recorded here when it lands.
