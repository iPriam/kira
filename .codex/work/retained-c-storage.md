# Retained C storage: resolved by owned C blocks

The deliberate process-lifetime leak this note used to bound is gone. Every
pointer Kira materializes for C is now a uniquely owned block (`Type::CBlock`)
that lives exactly as long as the Kira value holding it: cloned on a true copy,
freed on drop, moved on bind (C-layout structs with owning slots are
move-on-bind). A `retains: <param>` field on `@FFI.Extern` marks the callee
that keeps pointers past the call; that parameter consumes (`move` at the call
site) and transfers the argument's complete block tree to the engine's
retained registry — VM: freed with the instance; hybrid: with the session;
whole-process native: process teardown, counted by `kira_rt_cblock_retained_count`.

Cross-engine crossings carry blocks as `NativeStateValue::CBlock` /
`NativeCBlock` trees (payload + children by offset/width); the absorbing
engine rematerializes and patches addresses.

Ground truth: `kira_runtime_abi::c_storage` module docs, the FFI appendix's
"Foreign lifetimes", `tests-kik/ffi-harness/app/structs/FltLifetimeTests.kira`,
and `crates/kira-cli/tests/fixtures/ffi/ffi_program_cstring.kira`
(stash-and-recall proof of `retains:`).
