# Drop enforcement audit

Phase 2 landed user `Drop` (`.codex/work/trait-system-progress.md`). This note
records what that phase already enforced, what it left open, and what closed
each hole.

Evidence is a test name in `crates/kira-semantics/src/tests/drop.rs`, a case
prefix in `tests-kik/harness/app/` or `tests-kik/ffi-harness/app/`, or a code
path. Every row below was re-run against the tree, not read off the source.

## Audit

| # | Concern | Now | Evidence |
| --- | --- | --- | --- |
| 1 | `Copyable` + `Drop`, either colon order | Enforced | `KSEM297`, `a_drop_type_is_not_copyable` |
| 1 | `Copyable` + `Drop` at an `extend` site | Enforced | same path: `collect_conformances` records both spellings before `check_trait_conformance` |
| 1 | Claimed-`Copyable` struct with a `Drop` field | Enforced | `member_not_copyable` recurses into the field's type |
| 1 | `@Derive(Copy)` on a type reaching a `Drop` | Enforced | `KIR005`, `deriving_copy_on_a_type_reaching_a_drop_is_refused` |
| 2 | Reading a `Drop` value out of a struct field | Refused | `KSEM302`, `a_member_read_that_takes_the_value_is_refused` |
| 2 | Reading a `Drop` value out of an array element | Refused | `KSEM302`, `an_element_read_that_takes_the_value_is_refused` |
| 2 | Mutating method on a `Drop` field receiver | Refused | `KSEM302`, `a_mutating_method_on_a_member_is_refused` |
| 2 | Member-read base, borrow argument, non-mutating receiver | Legal, byte-identical | `a_member_read_no_position_owns_is_accepted`, `DrxAMemberReadLeavesTheValueWithItsOwner`, `DrxAnElementMemberReadCopiesNothing` |
| 2 | `xs.count` and `xs[i]` on an array of `Drop` elements | Legal, byte-identical | `DrxAnArrayOfDropValuesReadsWithoutTaking`; fixed in both backends, below |
| 2 | A member read out of a temporary | Refused | `KSEM302`, `a_member_read_out_of_a_temporary_is_refused` |
| 2 | A closure capturing a `Drop` `var` | Refused | `KSEM302`, `a_closure_capture_of_a_drop_value_is_refused` |
| 3 | `var x = D(); x = D()` runs the first body once | Enforced | `DrxOverwritingABindingKeepsOneValue`, `drxSection` order |
| 3 | `pair.first = D()` / `arr[0] = D()` | Enforced | `DrxWritingAMemberKeepsOneValue`, `DrxWritingAnElementKeepsOneValue` |
| 3 | A mutating method, direct and through `borrow mut` | Enforced | `DrxAMutatingMethodKeepsOneValue`; fixed in the VM, below |
| 4 | Struct holding a `Drop` value | Enforced | `TypeTable::runs_user_drop` follows fields; `a_type_holding_a_drop_value_runs_one_too` |
| 4 | Array of a `Drop` element | Enforced | `runs_user_drop` follows array elements |
| 4 | Enum payload holding a `Drop` value | Refused | `KSEM306`, `an_enum_payload_that_runs_a_body_is_refused`, and the generic and construct-backed cases beside it |
| 5 | Erasure into `Any` | Legal, byte-identical | erasing runs the body once on both engines, and the erased local is consumed (`KSEM107` on a later read) |
| 5 | Closure capture by value | Enforced | `KSEM117`: a capture must be trivially copyable |
| 5 | `Task { … }` spawn | Enforced | `KSEM159`: a task body takes `Int`/`Float` and returns `Int`/`Float`/nothing |
| 5 | `@Export` boundary | Refused | `KSEM303`, `an_exported_result_that_runs_a_body_is_refused` and the parameter case |
| 5 | `nativeState` / `nativeRecover` | Refused | `KSEM304`, `native_state_cannot_box_a_value_that_runs_a_body` and the recover case |
| 5 | `retains:` foreign parameter | Refused | `KSEM305`, `a_retained_foreign_argument_may_not_run_a_body` |
| 5 | An engine boundary | Refused | `KSEM307`, `a_drop_value_may_not_cross_between_engines` |
| 6 | Teardown | Abandoned, not stranded | `a_root_owed_a_drop_body_is_abandoned_rather_than_stranded`, `release_all_abandons_every_body_it_cannot_run` |
| 7 | Re-entrancy | Legal, byte-identical | a body that builds and drops another value, cascading fields, and a nested `Drop` all agree |
| 8 | `value.drop()` | Enforced | `KSEM300`, `calling_drop_by_name_is_refused` |
| 8 | `value.drop` as a function value | Enforced | `KSEM091`: a method is not a field |
| 8 | A second `drop` beside the conformance's | Enforced | `KSEM003` |
| 8 | A second `extend T: Drop` | Enforced | `KSEM290` |
| 8 | `drop` on a type that claims no `Drop` | Legal | an ordinary method; `drop` is not a reserved name |
| 8 | A `Drop` body declaring an engine | Refused | `KSEM301`, `a_drop_body_may_not_declare_an_engine` |

## Teardown decision

**Every user `Drop` body runs before the run that created the value ends, or the
value is refused at the boundary that would outlive it.**

Running parked bodies at teardown is not available. The VM parks a dropping
object because the heap cannot call Kira; the interpreter drains the park
between instructions. `Instance::release`, `Instance::release_all` and
`Instance::finish` hold no `HostCapabilities` and have no dispatch loop, so
there is no engine to enter a body with. The native side is the same shape: a
retained-registry free happens after the program's last frame.

So the guarantee is made statically. A type that runs a user `Drop` is refused
at each seam that can outlive the run or copy the value: the `@Export`
boundary, `nativeState`, a `retains:` foreign parameter, an enum payload, and an
engine boundary.

`Instance::release`, `release_all` and `finish` additionally abandon any parked
object, so a hand-built module that reaches one gives its storage back rather
than stranding it in the accounting `finish` reports.

## Engine decision

A `Drop` body declares no engine (`KSEM301`), and the LLVM backend compiles one
into the native half of a hybrid build whatever engine owns it
(`Codegen::carries_body`). A release is emitted from the type rather than from a
frame, so a release leaf in one half has no bridge to reach a body in the other
with. Both halves therefore hold the body and run the same source.

A value that runs a body may not cross between halves (`KSEM307`): the seam
marshals an aggregate tree, which is a copy built from bytes, and a value with a
body to run has no copy. Before this, a `Drop` value returned from `@Native` to
the VM lost its body entirely.

## Backend repairs

Each was a divergence between the engines, found by running the same program on
both:

* `xs.count` took the array out of its local. The VM ran every element body at
  the count and then trapped; native ran none at all. Both now borrow the base
  (`compile_borrowed_expr`, `lower_borrowed_expr`).
* A member read through an array element copied the element on native, running
  its body when the copy died. `Codegen::addressable` now reaches through an
  array element, so `cells[i].held.tag` walks to storage and copies only the
  member. `field_of_borrowed_element` was a narrower version of that walk and is
  gone.
* A mutating method on a `borrow mut` parameter copied the receiver on the VM
  and wrote it back, releasing the original at the store.
  `compile_writeback_argument` takes the local instead, so the write-back
  returns the value rather than replacing it.

## Forks taken

* **Refuse rather than represent.** A read that would give one value's storage
  two owners is refused, rather than growing field-sensitive partial moves. The
  excusal list (base of a further read, borrowed argument, non-mutating
  receiver) is what keeps ordinary reads working.
* **A temporary is not a place.** A read is excusable only when its base chain
  is rooted at a local, which is exactly the reach both backends can address.
  Anything else is a value the expression computed, and a struct read out of one
  would outlive nothing.
* **Only a struct read out of a temporary is refused.** An array or enum read
  hands back a share of one object, and releasing a share runs no body while the
  original holds it.
* **The export refusal fires at the signature, not at the class marker.** Only a
  parameter or a result reaches a consumer; an `@Export` class that appears in
  no exported signature hands nothing out.
* **The construct-backed refusal runs after field inference.** A backed
  declaration's member may be written without a type, so what it runs is not
  answerable until `resolve_construct_field_types` has run.

## Diagnostic codes

| Code | Concern |
| --- | --- |
| `KSEM300` | A direct call to a user `drop`. |
| `KSEM301` | A `Drop` conformance whose `drop` member is missing, misshapen, or declares an engine. |
| `KSEM302` | A read taking a `Drop` value out of the value that owns it, out of a temporary, or into a closure. |
| `KSEM303` | A `Drop` value at the `@Export` boundary. |
| `KSEM304` | A `Drop` value in callback state. |
| `KSEM305` | A `Drop` value at a `retains:` foreign parameter. |
| `KSEM306` | An enum variant payload that runs a user `Drop`. |
| `KSEM307` | A `Drop` value crossing an engine boundary. |

## Documentation

`sites/docs/content/docs/language-guide/memory-and-safety.mdx` carries the
guarantee, the read table, the refused positions, and the teardown rule.
