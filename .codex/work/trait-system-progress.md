# Trait system, phase 2

Phase 1 (the `Own C storage by the values that create it` and `Release lent call
temporaries newest-first` commits) built the conformance core: derived
`!Copyable`/`Drop` facts on the type table, drop glue in both engines, and the
internal non-denotable `Type::CBlock`. Phase 2 is the surface syntax and the
user-facing conformance machinery on top of it.

## Slice status

- **Slice 1 — trait declarations, conformance, static dispatch.** Landed.
- **Slice 2 — user `Drop`.** Not started.
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

## Test counts

`tests-kik/harness` moved from 1274 to 1287 cases, matching on vm and llvm. The
pin lives in `crates/kira-cli/tests/kik_harness.rs`.

## Sibling repositories

Nothing yet. Slice 3 changes the backed-declaration head, and the list of what
siblings must migrate is recorded here when it lands.
