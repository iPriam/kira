# Harness findings

Bugs surfaced while building and running the in-depth stress harness. Each has a
runnable reproduction. Severity is from the perspective of "real programs will
hit this".

Backends: `vm` (bytecode interpreter), `llvm` (native), `hybrid` (native dylib +
VM). Core Law #1 requires parity across all three.

---

## Already fixed in this work

### F0. Hybrid native struct/array result free — allocator/offset mismatch (FIXED)

A `@Native` function returning a struct by value (e.g. `rootViewportSize() ->
Size`) crashed the hybrid runtime on the first frame with SIGABRT ("pointer being
freed was not allocated"); native arrays returned to the VM had the same class of
bug. Root cause: the VM freed a `kira_struct_alloc` payload (8-byte type-id
header, pointer = base+8) at the wrong offset, and freed native arrays (allocated
by the installed VM allocator) with the C allocator. Fixed in
`packages/kira_vm_runtime/src/vm_native_bridge.zig`. This is why `../kira_ui`'s
`basic-demo` / `basic-kira-ui-app` now render in hybrid. Also fixed: the bounded
(`--quit-after`) runner masked child crashes as success
(`packages/kira_cli/src/commands/run.zig`). Regression tests:
`tests/pass/run/runtime_native_struct_value_return*`.

---

## Open

### F1. LLVM miscompiles a reassigned `var` enum local — FIXED

`repro: known-bugs/llvm_enum_var_local` (now correct on all backends);
regression test `tests/pass/run/enum_var_reassign_return` (vm/llvm/hybrid).

FIXED in `packages/kira_llvm_backend/src/backend_capi_drop.zig` +
`backend_capi_codegen.zig`: added a per-LOCAL cleanup slot for owned enum locals
(`enum_local_slot`, mirroring `copy_dest_slot` for structs). On an owned store the
local's slot is updated to the new heap enum (drop-before-overwrite), freed once
at exit, and escaped on return — so the runtime-current value is tracked across
branches instead of the compile-time last-lowered slot. Verified: repro
120/120/120, payload-enum variant correct, harness parity+leak clean, kira_ui
renders, corpus 222/223 (no new failures).

Original report:

A mutable enum local that is reassigned and then read back produces a wrong value
on the LLVM/native backend. VM and hybrid are correct.

```
kira run --backend vm     known-bugs/llvm_enum_var_local  -> 120  (correct)
kira run --backend hybrid known-bugs/llvm_enum_var_local  -> 120  (correct)
kira run --backend llvm   known-bugs/llvm_enum_var_local  -> 0    (WRONG)
```

An `if/else` reassignment form is off-by-one (119) instead of zero, so the bug is
sensitive to block structure. Returning the enum directly from each branch (no
intermediate `var`) is correct on all backends.

Root cause (confirmed): the LLVM drop elaboration
(`packages/kira_llvm_backend/src/backend_capi_drop.zig`) tracks ownership with
COMPILE-TIME arrays (`register_slot`/`local_slot`). Each `alloc_enum` gets its own
entry-block cleanup slot; `onStoreLocal` sets `local_slot[s] = register_slot[src]`.
When an enum `var` is reassigned in multiple branches, `local_slot[s]` becomes the
LAST-LOWERED branch's slot — which at runtime is the WRONG slot whenever a
different branch (or none) actually executed. At `return s` (the `.ret` lowering in
`backend_capi_codegen.zig` only specially escapes `.ffi_struct` via
`prepareStructReturn`; enums fall to `emitExitCleanup(src); ret`), `emitExitCleanup`
escapes only that one compile-time slot and FREES the others — including the heap
block actually holding the returned enum — so the caller reads freed memory
(garbage tag → exhaustive match finds no arm → returns 0; switch=0, if/else=off-by-
one). The existing `struct_contents`/`copy_dest_slot` path already uses a per-LOCAL
slot reused across reassignments; enums lack that. Silent wrong-answer, native-only.
A fix is in progress (worktree agent).

### F2. Hybrid leaks a struct-with-owned-array moved into a consumer — FIXED

`repro: known-bugs/hybrid_struct_array_move_leak` (now leak-clean on all backends)

FIXED in `packages/kira_vm_runtime/src/vm_interpreter_prologue.zig` (`bindArguments`):
in hybrid mode (`copy_struct_args_by_value=false`) an `.owned`/`.move` struct
param was unconditionally bound as borrowed, so a `move`d-in managed struct was
dropped by neither caller nor callee. It is now bound owned (dropped at frame
exit) when the incoming value is a managed VM struct; a native-layout struct (a
borrow handed in by native code, e.g. a sokol GraphicsFrame) stays borrowed.
Verified: leak gone (vm + hybrid `current=0`), kira_ui still renders leak-clean,
harness parity unchanged, full corpus unchanged (222/223, no new failures).

Original report:


A struct holding an owned `[Int]` field, moved into a by-value (owned) consuming
function, is never dropped in hybrid mode — leaking the struct shell and its
array on every iteration. The VM drops it correctly.

```
KIRA_RUNTIME_MEMORY_REPORT=1 kira run --backend vm     ...  -> heap arrays current=0   structs current=0    (clean)
KIRA_RUNTIME_MEMORY_REPORT=1 kira run --backend hybrid ...  -> heap arrays current=100 structs current=100  (LEAK)
```

The leak is in the VM heap (`nativeArrays`/`nativeStructs` stay 0), so it is the
VM-side drop of a moved-in owned struct param being skipped specifically under
the hybrid execution path (`copy_struct_args_by_value = false`). Suspect:
ownership/drop of owned aggregate params in the VM interpreter when invoked
through `kira_hybrid_runtime` vs. pure VM.

### F3. Hybrid double-free of a callback-returned enum moved into a struct (high)

`repro: tests/pass/run/runtime_native_enum_bridge` (currently RED on hybrid)

When a `@Runtime` function returns an enum to native (`pickShade() -> Shade`), the
runtime lowers it to a libc native block and retains it in
`pending_callback_native_enums` to free at teardown. But native moves that enum
into a struct field (`Swatch.shade`) whose destructor also frees it, so
`HybridRuntime.cleanupPendingCallbackReturns`
(`packages/kira_hybrid_runtime/src/runtime.zig`) double-frees it at exit:

```
kira run --backend hybrid tests/pass/run/runtime_native_enum_bridge/main.kira
  -> prints 1, 2 then "child terminated by signal 5" (double free at teardown)
```

Pre-existing (reproduces on the parent commit and on a clean tree). The
callback-return ownership model needs to decide that a return MOVED into a
native-owned structure is owned by native and must not also be freed by the
pending-returns cleanup.

### F4. Spurious diagnostic for attempt/handle + closure, with no location — FIXED

`repro: known-bugs/attempt_handle_closure_diag` (now checks clean);
regression test `tests/pass/run/attempt_handle_payloadless_variant` (vm/llvm/hybrid).

FIXED in `packages/kira_semantics/src/lower_stmts_attempt.zig`: the attempt->match
desugaring built a `destructure` pattern `Variant(binding)` for EVERY handle case
(binding defaulting to "_"), so a payload-LESS failure variant was destructured as
if it had a payload → match-lowering's KSEM105. Now a handle case with no binding
lowers to a bare-variant pattern. The companion "no source location" defect (FE4)
is fixed in `packages/kira_main/src/developer.zig` (+build.zig): `writeDiagnostics`
now routes through `diagnostics.renderer.renderAll` with the compiled source, so
`kira check`/`build`/`test` emit `--> file:line:col` with the source snippet.

Original report:

A package containing both an `attempt`/`try`/`handle` block and a closure with a
parameter emits a spurious `KSEM012: unknown local name 'x'` (or `KSEM105: match
pattern payload is invalid`) for valid code, and the diagnostic has NO source
location. Removing either construct clears it. The attempt/handle desugaring (to
enum `match`) interacts badly with closure capture / match-payload analysis. The
missing source location is a separate diagnostic-quality defect. The harness
works around this by using explicit `Result` matching, so attempt/handle is
currently under-covered.

---

## Frontend / compiler robustness (from the frontend-robustness sweep)

### FE1. Parser stack-overflow segfault on deep nesting — FIXED

FIXED in `packages/kira_parser/src/parser.zig` + `parser_types_exprs.zig`: added an
`expr_depth` guard (max 256) at the `parseExpression` chokepoint, through which
every level of expression nesting recurses. Pathologically deep input now yields a
clean located `error[KPAR014]: expression nesting too deep` instead of SIGSEGV.
Regression test `tests/fail/parser/expr_nesting_too_deep`. (FE2 — the analogous
unguarded recursion in semantic *lowering* on long flat chains — is still open; it
needs guards at multiple lowering recursion sites.)

Original report: The recursive-descent parser has no depth bound. ~1100+ nested parens
(`let x = ((((…1…))))`), or deeply nested array/struct/call/closure/`match`
expressions (~1500–3000 deep), SIGSEGV `kira check`/`kira ast` (stack overflow in
`packages/kira_parser/src/parser_types_exprs.zig`). Should emit a clean located
"nesting too deep" diagnostic instead of crashing.

### FE2. Semantic-lowering stack-overflow on long flat chains (high)

Distinct from FE1 (parser accepts these). ~700+ `+ 1` terms, or long unary
(`------1`), postfix (`.count.count…`), or ternary chains SIGSEGV during lowering
(`packages/kira_semantics/src/lower_program_enums.zig` `registerExpr`, unguarded
recursion over the expression tree). Needs a depth bound or iterative lowering.

### FE3. Empty control-flow body misparsed as a struct literal (medium)

`if true {}` reports `error[KPAR013]: struct literal requires a type name` — the
empty body `{}` is greedily consumed as an empty struct literal on the condition.
Same for `for/switch/match … {}`. With an identifier scrutinee the `{}` is silently
accepted as `v {}` and the parser then blames the token AFTER the body. Should
accept empty bodies or name the real issue and point at the empty body.

### FE4. `kira check` emits every diagnostic WITHOUT a source location — FIXED

Systemic: the developer facade `writeDiagnostics`
(`packages/kira_main/src/developer.zig`) rendered only code/title/help and never
the `--> file:line:col` label (the shared `kira_diagnostics` renderer already
supports it). FIXED together with F4: `writeDiagnostics` now routes through
`diagnostics.renderer.renderAll` with the compiled source so every `check`/`build`/
`test` error reports its span.

### FE5. Empty `match x {}` body — misleading diagnostic blaming the following token (low)

Like FE3: `match x {}` consumes `{}` as a struct literal and then reports
`KPAR001: expected '{' to start match body` pointing at the next token.

## Coverage gaps to expand next

- attempt/try/handle (blocked by F4)
- classes / inheritance dispatch (multi-parent, parent-qualified access)
- constructs (Widget/content/modifiers) execution where runtime-callable
- FFI extern calls + native-state round-trips driven from Kira
- string operations beyond `print`/`.count` (the language has no `+` concat;
  confirm what string surface is executable)
- deeper enum payloads (struct/array payloads) across backends
