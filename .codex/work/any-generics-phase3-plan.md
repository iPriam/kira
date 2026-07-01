# any -> monomorphized-generics migration (Phase 3 continuation)

Branch: `feat/any-generics-phase3` (fork `iPriam/kira`). Upstream draft PR: kira-lang-com/kira#20.
Remotes: origin = iPriam/kira (fork), upstream = kira-lang-com/kira (official). Never push work to upstream directly.

## Goal / decisions
`any` becomes a **monomorphized generic** (impl-Trait, anonymous per-position — a fresh anon param
at each occurrence, cannot be named/reused). Existential/dynamic dispatch moved to the **`some`**
keyword. User-locked: monomorphized (not existential), uniform, keyword = `some`, impl-Trait form.

## DONE (Phases 1-2, merged to fork main via #7; also in draft upstream #20)
- `some` existential keyword: AST `AnyTypeExpr.existential`; parser accepts `some`/`any` (contextual).
- Migrated all existential uses to `some`: @Content codegen (`existentializeContentType` in
  `lower_program_forms.zig`), 8 widget dispatch tests, extend-modifier self (`constructExistentialTypeExpr`).
- `some Int` -> KSEM097 "some requires a construct" (keyword-aware). Display paths (ast_dump, KSEM097) show keyword.
- @PropertyWrapper macro; diagnostics renderer synthetic-span crash fix; lower_program.zig split (Core Law #5).

## DONE (Phase 3, Slice 1, commit fc17229)
- Threaded `existential` flag AST -> `model.ResolvedType` -> `ir.ValueType`. Behavior-preserving, 378/0.

## GOTCHA (load-bearing)
`typeTextFromSyntax` MUST stay keyword-independent ("any" for both) — it feeds resolved type NAMES
used for coercion/virtual-dispatch matching. Surfacing "some" there desyncs matching and breaks
`Text -> some Widget` coercion (corpus catches it). Only diverge some/any in DISPLAY, never resolved name.

## REMAINING slices (sequenced)
2. Frontend bounds: `any Bound` accepts NON-construct bounds (Int, struct, protocol); only `some`
   requires a construct family. `validateAnyConstructType`/KSEM097 currently applies to both `.any`
   nodes — gate the construct requirement on `existential` (some) only.
3. Body typecheck: operations/methods on an `any Bound` value resolve against Bound's interface
   (construct-family methods for construct bounds; concrete type's methods for non-construct).
4. LLVM monomorphization: generalize `packages/kira_llvm_backend/src/backend_monomorphization.zig`
   (`functionNeedsMonomorphization`, `concreteParamType`) from construct_any-only to any
   `existential=false` construct_any, specialized to the actual arg type per call site.
5. VM: `any Bound` params take the concrete value (VM is dynamically typed at runtime -> observably
   identical to monomorphized native). Confirm `vm_construct_any.zig` path handles non-construct bounds.
6. Tests + docs: `any Int`/`any Widget` monomorphized vs `some Widget` existential, vm/llvm/hybrid
   parity; rewrite `tests/fail/semantics/any_requires_construct_{class,primitive}` +
   `tests/pass/check/any_construct_qualifier` (any-on-nonconstruct now VALID); migrate
   `../kira_ui/app/WidgetModel.kira` any->some. Keep #20 DRAFT until this is complete.

## Validate
`zig build` (snapshot), `zig build test-backends` (vm/llvm/hybrid corpus), `zig build test` (vm+units).
Disk was chronically tight; `.zig-cache` is safe to clear (forces cold rebuild).
