# Trait system, phase 2

Phase 1 (the `Own C storage by the values that create it` and `Release lent call
temporaries newest-first` commits) built the conformance core: derived
`!Copyable`/`Drop` facts on the type table, drop glue in both engines, and the
internal non-denotable `Type::CBlock`. Phase 2 is the surface syntax and the
user-facing conformance machinery on top of it.

## Slice status

- **Slice 1 — trait declarations, conformance, static dispatch.** Landed.
- **Slice 2 — user `Drop`.** Landed.
- **Slice 3 — backed-declaration respelling.** Landed.

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

### The parameter list tells a family from a declaration

`construct Name(params) extends Family { … }` replaces the bare
`Family Name(params) { … }` head, and the old head is gone: no identifier begins
a declaration any more, so every declaration form opens with a keyword. The
parameter list is what separates the two forms, and `extends` means one thing in
both — the declaration this one is written against.

The macro scanner reads the same rule: `DeclarationKind::Form` is a `construct`
whose name is followed by `(`, and its `family` comes off the `extends` clause
rather than off the token before the name.

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
| `KPAR077` | A construct with a parameter list and no `extends` clause. |
| `KPAR078` | A backed declaration naming more than one family. |

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

Slice 3 replaced the bare `Family Name(params) { … }` declaration head with
`construct Name(params) extends Family { … }`. This repository is migrated;
siblings are not, and their `.kira` sources will not parse until they are.

Every sibling repository holding `.kira` sources — `../kira-ui` and any other
checkout outside this one — must rewrite each backed declaration:

```kira
Widget Text(content: String) { … }                     // before
construct Text(content: String) extends Widget { … }   // after

Test SumsToTen { … }                                   // before
construct SumsToTen() extends Test { … }               // after
```

The rule is mechanical, and both spellings of the old head are covered by it:

- `Family Name(params) { … }` becomes
  `construct Name(params) extends Family { … }`.
- `Family Name { … }` becomes `construct Name() extends Family { … }` — the
  parentheses are what marks a backed declaration now, so a head without them
  is read as a family.

Nothing else about the body changes, and `construct Family { … }` and
`construct Child extends Parent { … }` are untouched. A `package.kira`
manifest's `Package Name { … }` head is not a construct and is untouched too.

The rewrite is one head per file-scope line, from column zero to the `{` that
opens the body, and the parameter list may span lines. Kira source embedded in
Rust test strings needs the same rewrite; this repository had 169 of those
beside the 1614 in `.kira` files.

**Migrated 2026-08-24.** All seven sibling repositories are rewritten,
building, and committed: ui-foundation `128cbcb`, kira-graphics `7016b58`,
kira-layout `d0cb069`, kira-ui `1c282b9`, ui-motion `baa6a36`, opacity-ui
`3cc70fc`, project-matter `b384398` (603 heads across 90 files). Two facts
the migration surfaced, both fixed in this repository alongside it: autobind's
libclang needed the Apple `-isysroot` exactly as `native_sources` did (dawn's
webgpu.h found no `<math.h>` once the cache invalidated), and a test package
now has to own the `Test` family and its collector runner — project-matter's
harness gained both files and reports 170/170 on the VM driver.

# Trait system, phase 3

## Slice status

- Slice 1a, supertraits — committed `6cd9027`.
- Slice 1b, `Send`/`Sync` compiler-known markers — committed `67a94dd`.
- Slice 2, constructs re-anchored on the conformance table — this commit.
- Slice 3, trait existentials — next.
- Slice 4, generic bounds — after 3.

## Design decisions taken here

### A supertrait is an obligation, discharged once

`trait Ordered: Equated { … }` makes conforming to `Ordered` demand an
`Equated` conformance from the same type — written in its colon list or an
`extend` block, or discharged structurally when the supertrait is one of the
compiler-known derived markers. Defaults may call supertrait members on `self`,
because by the time a default runs the receiver keeps both promises. Cycles are
`KSEM309`; a clause naming a non-trait is `KSEM308`.

### `Send` and `Sync` are derived facts about a shape

Both join `Copyable` as compiler-known derived markers: a written claim is a
checked assertion against the type's own members (`KSEM311`), never data
inheritance. The base facts, reasoned from what each leaf actually is:

| Leaf | Send | Sync | Why |
| --- | --- | --- | --- |
| aggregate (struct/class/enum/construct-backed) | all fields Send | all fields Sync | structural, like `Copyable` |
| capture cell (`var` capture) | no | no | the language's shared mutable box; both engines write through it without a lock |
| native-state token | no | no | names a store the minting engine owns; elsewhere it is a number |
| task handle | no | no | a row in an executor's table, which no other thread holds |
| C block | yes | no | uniquely owned foreign storage: movable, but a second concurrent holder would be a second owner of storage the foreign side may have freed |
| `RawPtr` / `ForeignPtr` | yes | yes | an opaque word Kira never dereferences, frees, or computes on; thread-safety of the pointee is the foreign declaration's contract, stated where the call is |
| function type | no | no | its fields are the program-wide join over closure captures, not final until every literal is lifted — a type that cannot promise what its values kept promises neither |

The VM's `Rc` internals are deliberately absent from the table: they are
engine-internal bookkeeping, and a rule that read them would deny a plain
`Point` the right to cross a thread.

The concurrency seam layered on top is today's narrow task surface: creating a
task checks that everything it captures and returns is `Send` (`KSEM312`),
beside the existing `KSEM159` restrictions. The check reads only the trait
table, so it stays correct if the task surface widens; the current interaction
is pinned by tests rather than redesigned away.

### A family claim files per-declaration conformances

`construct Widget: Hashable { … }` and `extend Widget: Hashable { … }` both
record one [`Conformance`] per backed declaration, tagged `via_family`, filed
at that declaration — where a refusal's fix goes. Members the family provides
at its own scope satisfy the variants at once. A declaration's own colon-list
claim wins over the family's. The family's contract half (`@Required`) now
records its rows in the same table traits use, so one engine answers "does this
type satisfy the surface" for both. `KSEM298` survives narrowed: it is now the
diagnostic for a claim that cannot conform at all, chiefly a compiler-known
trait claimed by a template that has no members of its own.

## New diagnostic codes

| Code | Meaning |
| --- | --- |
| `KSEM308` | A supertrait clause naming something that is not a trait. |
| `KSEM309` | A cycle of supertrait clauses. |
| `KSEM310` | A claimed trait whose supertrait obligation is unmet. |
| `KSEM311` | A `Send`/`Sync` claim the type's own members refute. |
| `KSEM312` | A task boundary crossing a value that is not `Send`. |

## Test counts

Harness moved 1300 → 1308 cases, byte-identical tallies on VM and LLVM; ffi
harness steady at 274. Semantics unit suite at 747.

## Gate note

`kira_dev_validate` (MCP) is unavailable in this session; the gate was run as
the four CLI gates from AGENTS.md — `cargo build --workspace`, `cargo test`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` —
plus the kik harness parity runs (VM vs LLVM checksums, ffi on hybrid) and
`KIRA_FOUNDATION_HOME=$PWD/foundation kira lint ../ui-foundation`.

### Slice 3 — trait existentials

A trait's name in a type position (`let x: Scored`, parameters, results,
array elements) is an **existential over its conformers**. The representation
generalizes the construct-family machinery rather than inventing a second box:
analysis synthesizes `some Scored = Leaf(Leaf) | Token(Token) | …` — one
variant per distinct conforming type, in first-recording order from the
conformance table — and a member call through the value lowers to the same
balanced tag-tree dispatcher families build, each arm calling the concrete
implementation that type's conformance provided. No new opcodes, tags, or wire
surface: both engines execute ordinary enum projection, branching, and direct
calls, unchanged.

Decisions taken here:

- *Membership is the table.* A variant exists exactly when the conformance
  table has a row for `(trait, type)` — direct claims, impl blocks, and family
  claims alike. Supertrait discharge is answered by the checker on demand
  (`KSEM310`) and does not mint rows, so membership stays "what was written".
- *Object safety.* Every member must be reachable through a value: a trait
  whose member takes no `self` is refused at the type position with `KSEM313`
  naming the first offending member. A member's parameters may themselves be
  other existentials; nothing else restricts the shape.
- *Compiler-known traits have no existential.* `Copyable`/`Drop`/`Send`/`Sync`
  state facts about one type's own members or body and classify no values, so
  their names in type positions keep `KSEM295`, reworded for the new rule.
- *Equality.* An existential compares as the enum it is: same concrete variant
  and equal payloads are equal; different conformers are unequal even when
  their payloads compare equal. Same rule families already follow.
- *Drop.* The existential IS an enum value, so single-owner enum rules apply
  untouched: wrapping moves, the payload releases once through the ordinary
  enum path. Sound without a refusal; `KSEM303` is left exactly as it was.
- *Reservation is lazy, fill is two-phase, dispatchers fixpoint.* The enum id
  is minted the first time the name resolves in a type position (signatures
  resolve before conformances), variants and member shapes fill after
  `collect_conformances`, and a reservation that lands later (a body that
  mentions the trait first) fills itself at its first call or coercion.

New codes: `KSEM313` object-unsafe trait in type position; `KSEM314` unknown
member on an existential; `KSEM278` reused verbatim for trailing-content on a
member that takes none.

Harness moved 1308 → 1315 cases, byte-identical VM/LLVM tallies.

### Slice 4 survey — where type parameters exist today

Read before implementing, per the slice's ground rules.

**Declaration forms with type parameters: `enum` only.** `parse_type_params`
(`crates/kira-parser/src/generics.rs`) is called for real on exactly one path,
`Parser::parse_enum`. Every other form refuses the list by name with `KPAR047`:
`struct` (`aggregate.rs`), `class` (`aggregate.rs`), `trait`
(`traits.rs`), free and method `function` (`item.rs`, `construct/members.rs`).
A `construct` header has no parameter-list refusal at all — a `<` after its
name falls into the header-clause skip and dies on the expected `{`; there is
no generic construct surface to extend. Type *arguments* parse everywhere a
type does (`Name<Args>` → `TypeRef::Generic`) and in one expression position,
`nativeRecover<T>` — neither makes a declaration generic.

**Execution model: monomorphization at analysis time, on every engine.** A
generic enum declares no type; it is registered as a template
(`Analyzer::generic_enums`). Each written instantiation substitutes the
arguments into the template's body and declares an ordinary enum row under the
mangled name `Result<Int, AppError>` (memo key = the name), recorded in
`EnumTable::instantiations` so widening can find it later. Nothing below
semantics learns generics exist: both engines see plain enums, dispatch reads
tags by id, and no opcode, IR node, wire format, or runtime tag carries
parameters. Erasure happens only at the *use* of a widened value (the
rebuild-to-`Any` rule), not as the representation of generics.

**Consequences that decide slice 4's shape:**

- Bounds land on enum type parameters, because that is the only place
  parameters exist. Function-level generics do not exist today and are not
  grown here; `KPAR047` stays for struct/class/function/trait/construct.
- A bound discharges at instantiation, inside the existing monomorphizer — no
  second execution model, no backend work at all.
- Instantiation can be triggered while declaration passes are still running
  (a struct field naming `Boxed<Int>` mints during `collect_structs`, before
  `collect_conformances`), so the discharge check cannot run inline. It is
  queued at instantiation and answered after the conformance table and drop
  glue are final — the same record-now/answer-later shape
  `enum_payload_sites` uses for `KSEM306`.
- An enum body carries no code over parameter values: the only expressions in
  a template are variant payload defaults, which *produce* a value rather than
  hold one, and analyze against the substituted concrete type. So
  "bound members callable on parameter values inside the scope" has no
  reachable surface yet; it binds the moment a generic form with bodies lands.

### The bare family-conformance head is language again

`Family Name { … }` parses as a zero-parameter declaration backed by
`Family` — the same tree `construct Name() extends Family { … }` produces,
with the family named where the keyword would be and the clause implied by
the position. The parser turns any two-identifier-plus-body head into the
backed form; a pair carrying a parameter list stays refused, because a
parameter list is what makes the spelled-out form's kind decidable. Computed
members discharge `@Required let`s through it unchanged. Grammar gained
`family_conformance_declaration` with its corpus case; harness pins the
discharge end to end.
