# FFI harness findings

Bridge bugs surfaced while building the FFI / native-bridge `Test` suite.

## FF1. Distinct captured closures crossing the bridge dispatched to the wrong body — FIXED

When several VM closures that each capture a scalar `let` were passed across the
hybrid bridge to `@Native` (in one call or in sequence) and invoked, a later
closure ran an earlier one's body. Root cause (NOT a function_id collision):
`Vm.exported_native_closures` cached the exported native block keyed by the VM
`closure_ptr`. A closure passed by `move` is consumed and its pointer freed; a
later, different closure can be allocated at the SAME address, so the
pointer-keyed cache returned the first closure's stale native block (body AND
captures) — `inv(move cbA,1)` then `inv(move cbB,1)` gave `11, 11` instead of
`11, 101`. FIXED by dropping the pointer-keyed dedup: `exportRuntimeClosureToNative`
now always exports a fresh native block, and `exported_native_closures` is a plain
list registry freed at `deinit` (`vm.zig`, `vm_native_bridge.zig`).

Regression: the 12 previously-excluded capture tests in
`app/closures/FcbCallbackTests.kira` (Group K) are re-enabled and pass, including
`FcbTwoDistinctCaptures`/`FcbThreeCaptures`/`FcbSequentialCaptures` (distinct
captured closures in one call and in sequence). Leak-clean.
