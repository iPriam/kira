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

### F3. Hybrid double-free of a callback-returned enum moved into a struct — FIXED

`repro/regression: tests/pass/run/runtime_native_enum_bridge` (now PASSES on hybrid).

FIXED across `packages/kira_llvm_backend/src/backend_capi_drop.zig` (enum call/
call_value/call_virtual results are now drop-tracked in hybrid too, not only
llvm_native) + `packages/kira_hybrid_runtime/src/runtime.zig`
(`cleanupPendingCallbackReturns` no longer frees `pending_callback_native_enums`).
A `@Runtime`-returned enum is lowered to a libc-allocated native block
(`lowerEnumToNativeOwned`) that the native CALLER owns and frees exactly once — via
its own scope-exit drop when transient, or via the containing struct's
`release_contents` when moved into a field. The runtime previously ALSO freed it at
teardown → double free. Native now consistently owns+frees these (which also
reclaims the per-frame `graphicsEventKindFromRaw` enum leak the old hybrid `continue`
left). The managed VM enum stays retained in `pending_callback_return_values` for
borrowed-payload lifetime. Verified: enum-bridge clean (1/2), native blocks freed
(nativeStructs=0), kira_ui renders, corpus green (enum-bridge now passes; only the
libffi env skip remains).

Original report:

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

## Open (lower priority)

### F6. Unbounded `pending_callback_return_values` growth in hybrid (medium)

`HybridRuntime.pending_callback_return_values` retains the managed VM value of EVERY
`@Runtime`-callback return for the whole runtime lifetime (dropped only at deinit;
`trimPendingCallbackReturns` is a no-op). A program returning aggregates from
callbacks accumulates them: 500 enum returns → `structs current=1500` held until
exit. Freed at teardown (so not a leak at process exit), but a real per-run memory
growth — for a long-running UI app returning values from per-frame callbacks it
grows without bound. Pre-existing (predates F3). A proper fix needs the retention
trimmed once native no longer references the value (e.g. confirm
`lowerEnumToNativeOwned`/struct/array lowering COPIES the payload rather than
aliasing the managed value, then drop it immediately instead of retaining).

### FE2. Semantic-lowering stack overflow on long flat chains (high, open)

(see the Frontend section) — still open; needs depth guards at the lowering
recursion sites (`registerExpr`, the main expr lowering, ternary lowering).

## Runtime sweep findings (4-finder run-only sweep)

### S1. Hybrid corrupts stdout when redirected to a file — FIXED

`kira run --backend hybrid DIR > out.txt` wrote only a fragment (e.g. `\nCC` for
three prints) — silent data loss; pipes/TTYs masked it. `DirectStdoutWriter`
(`packages/kira_hybrid_runtime/src/runtime.zig`) built a fresh POSITIONAL
`File.writer()` per call, each writing from logical offset 0 and clobbering a
seekable destination. FIXED: use `writerStreaming` (advances the shared fd offset).
Verified: file output now byte-identical to vm/llvm; pipe unaffected.

### S2. VM/hybrid drop stores & mutations to array-element places ≥2 projections deep — FIXED

FIXED via a new `packages/kira_ir/src/lower_from_hir_places.zig` (lowerMutableObject /
WritebackList / emitWritebacks / lowerDirectCallArgs) wired into
`lower_from_hir_program.zig` (deep field/index assignment) and `lower_from_hir.zig`
(nested `.append` receiver + `borrow mut` array-element call args). Any place rooted
at an array index now read-modify-write-backs the whole element (array_get → apply
projections to the copy → array_set), so `arr[i].a.b = v`, `arr[i].xs[j] = v`,
`arr[i].xs.append(v)`, and `bump(arr[i])` (borrow mut) all persist. Verified: all
variants + a 2000-iter stress are identical across vm/llvm/hybrid and leak-clean;
corpus green. Regression tests: tests/pass/run/array_element_{deep_field_store,
nested_append,borrow_mut_writeback,mixed_projection}_parity.

Original report:

`repro`: `arr[0].inner.x = 77` prints `1` on vm/hybrid, `77` on llvm. The VM resolves
the array-index base `arr[i]` to a throwaway COPY for any place nested 2+ levels
below the index, so the store/mutation is applied to the copy and discarded — silent
data loss, no diagnostic. Confirmed variants (same root cause): `arr[i].a.b = v`,
`arr[i].xs[j] = v`, `arr[i].xs.append(v)` (count stays 0; a later index aborts
"array index out of bounds"), and `borrow mut` of an array element
(`bump(arr[i])` where `bump(r: borrow mut Row)` never writes back). Single-projection
(`arr[i].n = v`, `arr[i].whole = struct`) works; the same deep stores on a plain
local struct work. LLVM is correct throughout. This is a high-impact VM
place-resolution bug (common patterns) affecting both vm and hybrid; the affine
backend lvalue path handles it but the VM interpreter does not. Likely in the VM's
store/append/borrow-arg place resolution for array-element-rooted projections.

### S6. VM/hybrid interpreter stack-overflow on moderate recursion (~350+ frames) (high, OPEN)

`rec(1000)` (simple `1 + rec(d-1)`) crashes the VM interpreter (SIGABRT/SIGSEGV, no
diagnostic) while llvm returns 1000. The bytecode interpreter recurses on the native
stack per Kira call frame with no depth bound. Needs a recursion-depth guard in the
VM interpreter that raises a clean runtime error (the analogue of FE1 for the VM,
and related to FE2). Real programs with deepish recursion hit this.

### S7. Property access on a call's temporary fails to lower (KIR001) — FIXED

Reading a field/property directly off the value returned by a call — e.g.
`strPick(i).count` where `strPick(i) -> String` — failed to lower with
`KIR001: a lowering obligation was left undischarged`. Root cause: the
function-reachability walk (`lower_from_hir_program.zig markReachableExpr`)
handled `.array_len`/`.field` but had no `.string_len` case, so a `String`
`.count` whose object is a call fell through to the no-op `else` and never
marked the callee reachable → never lowered. FIXED by adding the missing
`.string_len` case (commit b05596d); regression test
`tests/pass/run/call_temp_string_count_parity`. `call().count` now lowers on
vm/llvm/hybrid.

### S8. Negative `%` uses floored modulo while `/` truncates toward zero (medium, OPEN)

Surfaced by the `kira test` suite migration. `(0 - 17) % 5` returns `3`
(Python/floored modulo) but `(0 - 17) / 5` returns `-3` (truncated toward zero),
so the standard division identity `(a / b) * b + (a % b) == a` is BROKEN for
negative `a`. `/` and `%` must agree on rounding. Kira models Rust affine
ownership and Rust uses truncated modulo (`-17 % 5 == -2`), so the fix is to make
`%` truncate toward zero to match `/` — and it must be fixed across BOTH the VM
interpreter and the LLVM backend for parity. Minimal repro:
`test { return (0 - 17) % 5 } expect { Result.Ok(0 - 2) }` fails (gets 3).

## Coverage gaps to expand next

- attempt/try/handle (blocked by F4)
- classes / inheritance dispatch (multi-parent, parent-qualified access)
- constructs (Widget/content/modifiers) execution where runtime-callable
- FFI extern calls + native-state round-trips driven from Kira
- string operations beyond `print`/`.count` (the language has no `+` concat;
  confirm what string surface is executable)
- deeper enum payloads (struct/array payloads) across backends
