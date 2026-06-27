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

### F1. LLVM miscompiles a reassigned `var` enum local — WRONG RESULT (high)

`repro: known-bugs/llvm_enum_var_local`

A mutable enum local that is reassigned and then read back produces a wrong value
on the LLVM/native backend. VM and hybrid are correct.

```
kira run --backend vm     known-bugs/llvm_enum_var_local  -> 120  (correct)
kira run --backend hybrid known-bugs/llvm_enum_var_local  -> 120  (correct)
kira run --backend llvm   known-bugs/llvm_enum_var_local  -> 0    (WRONG)
```

An `if/else` reassignment form is off-by-one (119) instead of zero, so the bug is
sensitive to block structure. Returning the enum directly from each branch (no
intermediate `var`) is correct on all backends. Suspect: enum_instance local
store/overwrite drop elaboration in
`packages/kira_llvm_backend/src/backend_capi_drop.zig`
(`onStoreLocal`/`freeSlot`) and the entry-block enum local slot in
`backend_capi_codegen.zig`. This is a silent wrong-answer parity bug, not a crash.

### F2. Hybrid leaks a struct-with-owned-array moved into a consumer (high)

`repro: known-bugs/hybrid_struct_array_move_leak`

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

### F4. Spurious diagnostic for attempt/handle + closure, with no location (medium)

`repro: known-bugs/attempt_handle_closure_diag`

A package containing both an `attempt`/`try`/`handle` block and a closure with a
parameter emits a spurious `KSEM012: unknown local name 'x'` (or `KSEM105: match
pattern payload is invalid`) for valid code, and the diagnostic has NO source
location. Removing either construct clears it. The attempt/handle desugaring (to
enum `match`) interacts badly with closure capture / match-payload analysis. The
missing source location is a separate diagnostic-quality defect. The harness
works around this by using explicit `Result` matching, so attempt/handle is
currently under-covered.

---

## Coverage gaps to expand next

- attempt/try/handle (blocked by F4)
- classes / inheritance dispatch (multi-parent, parent-qualified access)
- constructs (Widget/content/modifiers) execution where runtime-callable
- FFI extern calls + native-state round-trips driven from Kira
- string operations beyond `print`/`.count` (the language has no `+` concat;
  confirm what string surface is executable)
- deeper enum payloads (struct/array payloads) across backends
