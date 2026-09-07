# Survey: callable signatures and generic inference (2026-09-01)

Six signature representations, no canonical one:
- `FuncSig` (kira-semantics analyze/signatures.rs:17-41): types + per-param ownership + main/main-thread
  flags; receiver ownership hardcoded BorrowRead (94-98); defaults in side table `param_defaults`
  (211); mutability via `mutating_methods` fixpoint (mutation.rs:79); no labels/async/Execution/
  result ownership/failure.
- `HirFunction` (kira-semantics-model hir.rs:233-269): ownership only on leading `HirLocal`s.
- Function types: no `Type::Function`; synthesized struct via `closures/function_values.rs:15-67`,
  key `(params, ownership, result)` (closures/mod.rs:327); no labels/defaults/async/@MainThread.
- Trait `RequiredShape` (traits/check.rs:35-50): types + receiver_mutates only; `check_member`
  (274-392) matches by exact `Vec<Type>` equality (326-329), result `!=` (354), receiver via
  inferred `mutates_self` (374-391). Never compares param ownership/labels/defaults/async/affinity.
- `ExistentialMethod` (traits/existential.rs:45-58) same erasure; `FamilyMethodShape`
  (constructs/dispatch.rs:22-36) carries ownership+defaults (good template).
- FFI `ForeignSignature` (runtime-abi foreign.rs:224-233) + `HirForeign` (hir.rs:132-181) with six
  parallel Option arrays; callback fit types-only (foreign_callback.rs:111).
- `TaskTarget` (hir/exprs.rs:701-707); tasks.rs:130-196 re-derives from FuncSig, Int/Float only,
  `assignable_to` not `admits` (182).
- Classes: no vtable; copies per subclass (analyze/callable.rs:113-152), cap
  `SPECIALIZATION_LIMIT=64` (161) silently falls back to parent (138-140, doc 154-160); call site
  by mangled name `feed$1$Dog` (typeck/calls/targets.rs:115-134).

Generic inference: `infer_type_ref_inner` (generics/functions.rs:190-254) handles Named/Array/
Generic only, `_ => {}` at 252; first-write-wins, `!=` conflict; `instantiate_generic_function`
(25-148) bails on receivers (35-37), discards trailing closures (146), ignores expected result;
`try_argument_types` (typeck/overloads.rs:139-159) probes with no expectation (closures → Error);
explicit `f<T>()` only for generic free functions with no overloads (targets.rs:24); failures all
KSEM316 at declaration span; aggregates `generic_aggregate_for_call` (generics/aggregates.rs:476-606)
expected-type shortcut without validation (486-493).

Widening: `widens_to` enums only (ty/widening.rs:70-78); Result<Int,Child>→Parent already refused;
remaining implicit: Any erasure (ty/mod.rs:260), enum→Any Widen, class Child→Parent in argument
positions (`admits_argument` coercion.rs:68-91). `admits` vs `assignable_to` inconsistent across
closures/calls.rs:160, tasks.rs:182, foreign/call.rs:187, constructs/slots.rs, typeck/env.rs:58,
typeck/compiler.rs:59, stmt/attempts.rs:240.

Existentials: reserve (existential.rs:68-118) + fill at run.rs:159 (before signatures at 170);
lazy re-fill (209-240); dispatchers built in fixpoint run.rs:283-291; closures finalize run.rs:303.
