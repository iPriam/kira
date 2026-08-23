# Wasm callback shims: the unimplemented split

## State

`kira_llvm_backend::callback_thunk_symbol` always returns `callback_name`
(`kira_ffi_callback_N`), for every target and every signature, despite a doc
contract and `shim::callback_needs_entry` describing a split.

## Why it is not split today

The LLVM side emits one body per callback with libffi's closure signature
`(cif, result, arguments, user_data)` (`codegen/callback.rs`). The generated C
shim's entry has the true prototype and forwards positionally:
`body(p0, &p1)`. Those two ABIs are incompatible, so pointing the shim's
forward at the closure-signature body would misread every argument. Today the
collision surfaces as a duplicate-symbol link error on wasm for any
`@FFI.Callback` taking a struct by value — loud, not silently wrong.

## What finishing this needs

1. A second LLVM emission path: a true-prototype body (scalars by value,
   aggregates as pointers) that reads arguments directly instead of out of
   libffi's array.
2. `callback_thunk_symbol` honoring `callback_needs_entry`: shim entry keeps
   `kira_ffi_callback_N`, body lands under `kira_ffi_callback_body_N`.
3. Wasm materialization of a callback address that does not go through
   `kira_rt_ffi_closure` (bundled libffi reports emscripten unsupported):
   take `&kira_ffi_callback_N` from the shim object instead.
4. Harness coverage in ffi-harness for struct-by-value callbacks on wasm.

Found by the 2026-08 bug hunt (hybrid/LLVM M2).
