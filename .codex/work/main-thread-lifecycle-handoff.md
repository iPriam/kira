# Main-thread lifecycle handoff

## Objective

Finish `@MainThreadLifecycle` across parser, semantics, HIR/IR, bytecode, VM, LLVM/native, hybrid, `tests-kik`, tree-sitter, and `sites/docs`.

Do not stop at a green subset. Final proof must include:

- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --all-targets --no-fail-fast`
- backend parity
- exact VM, LLVM, and hybrid lifecycle harness output
- tree-sitter tests

## Final language model

- `@Main` remains the application entrypoint and runs on the helper/application thread.
- `@MainThreadLifecycle` marks a separate zero-argument `Void` function.
- Calling a lifecycle schedules a new instance on the operating-system main thread and returns `Void`. It does not run the body on the application thread.
- Each started instance owns a preserved stack and locals across cooperative slices.
- Multiple lifecycle instances share main-thread CPU time. One lifecycle receives every lifecycle slice; several take turns.
- The lifecycle's ordinary transitive call tree remains pinned to the main thread.
- `@MainThread` targets coexist with lifecycles and remain available through `MainThread.invoke`, `spawn`, `post`, and task `.await`.
- A stalled application thread cannot stop an already-started lifecycle because the application and main loop are separate OS threads.
- Returning ends one lifecycle instance. The process main loop remains alive after `@Main` returns until every started lifecycle returns.
- WebAssembly refuses `@MainThreadLifecycle` and `@MainThread` with `KSEM338` because no Kira-owned process main thread exists there.

Rejected models:

- Lifecycle replaces `@Main`.
- Runtime auto-starts every annotated declaration.
- Only one lifecycle may exist.
- Lifecycle and `@MainThread` targets cannot coexist.
- Lifecycle state restarts from the function entry every turn.

## Implemented compiler model

- `HirProgram.main_thread_lifecycles` and `IrProgram.main_thread_lifecycles` are function-id vectors.
- Lifecycle declarations remain independent from `program.main`.
- Direct lifecycle calls lower to `HirExpr::MainThreadCall` and `IrExpr::MainThreadCall` with `MainThreadOp::LifecycleStart`.
- `MainThreadOp::LifecycleStart = 3` was appended to the wire enum.
- The existing `MainThreadCall` bytecode instruction carries lifecycle starts, so no new instruction opcode was needed.
- `Instruction::MainThreadLifecycle` remains function metadata at instruction zero.
- `KSEM343`, which refused lifecycle calls, was removed.
- `KSEM341` rejects lifecycle parameters.
- `KSEM344` rejects a non-`Void` lifecycle result.
- `KSEM339` rejects one function carrying both `@Main` and `@MainThreadLifecycle`.
- `KSEM340` and the old single-lifecycle `KSEM342` rule are removed.

Key semantic files:

- `crates/kira-semantics/src/analyze/signatures.rs`
- `crates/kira-semantics/src/analyze/function.rs`
- `crates/kira-semantics/src/typeck/calls/arguments.rs`
- `crates/kira-semantics/src/tests/main_thread.rs`
- `crates/kira-semantics-model/src/hir.rs`
- `crates/kira-ir/src/ir.rs`
- `crates/kira-ir/src/lower.rs`
- `crates/kira-runtime-abi/src/main_thread.rs`

## VM scheduler

- `crates/kira-vm-runtime/src/fiber.rs` owns preserved VM heap, scratch state, frames, and lifecycle completion.
- Interpreter dispatch accepts an instruction budget and returns `Dispatched::Suspended` without destroying frames.
- Main loop starts fibers only after a `LifecycleStart` request. It no longer auto-starts declarations.
- Each main-loop pass gives every active VM fiber 4096 instructions, pumps external native fibers, then services deferred `@MainThread` work.
- A lifecycle remains alive after helper completion until its own function returns.
- Helper-channel disconnect no longer tears down active lifecycles.
- Trapped fibers are marked finished and never re-entered.

Key VM files:

- `crates/kira-vm-runtime/src/interp.rs`
- `crates/kira-vm-runtime/src/interp/frames.rs`
- `crates/kira-vm-runtime/src/interp/main_thread.rs`
- `crates/kira-vm-runtime/src/main_thread.rs`
- `crates/kira-vm-runtime/src/main_thread_tests.rs`

`crates/kira-vm-runtime/src/main_thread.rs` is currently 763 lines. Repository instructions require `.rs` files to be split below 700 lines. Split this file before completion, preserving behavior and APIs.

## Native scheduler

Chosen design: stackful cooperative fibers rather than `ucontext` or LLVM coroutine splitting.

Reasons:

- No signal-mask syscall per switch.
- Preserves the full transitive call stack without coloring every call as an LLVM coroutine.
- Checkpoints occur only in lifecycle-reachable function trees and loop tests.
- Unix fibers switch directly between scheduler and lifecycle contexts using callee-saved register and stack-pointer swaps.
- Unix stacks come from one contiguous virtual arena with guard pages. Each stack reserves 256 KiB; up to 256 simultaneous instances are supported.
- Windows uses the OS fiber API so TEB stack bounds, guard growth, and SEH remain correct.

Files:

- `crates/kira-native-bridge/src/main_thread/fiber.rs`
- `crates/kira-native-bridge/src/main_thread/fiber/unix.rs`
- `crates/kira-native-bridge/src/main_thread/fiber/windows.rs`
- `crates/kira-native-bridge/src/main_thread.rs`
- `crates/kira-llvm-backend/src/reachability.rs`
- `crates/kira-llvm-backend/src/codegen/lower/mod.rs`
- `crates/kira-llvm-backend/src/codegen/lower/stmt.rs`
- `crates/kira-llvm-backend/src/codegen/entry.rs`
- `crates/kira-llvm-backend/src/codegen/types/runtime.rs`
- `crates/kira-llvm-backend/src/codegen/types/runtime_declare.rs`

Generated native code now:

- Emits `kira_main_thread_lifecycle_resolve(function_id)`.
- Installs the resolver before entering the helper/main loop.
- Emits lifecycle checkpoints in lifecycle-reachable loop tests.
- Retains lifecycle roots during native reachability.

The native runtime exports local start, pump, and reset functions so a VM-owned hybrid main loop can schedule native lifecycle fibers without nesting a second OS-thread event loop.

## Hybrid scheduler

- Hybrid native-entry programs use the native helper/main loop.
- Hybrid runtime-entry programs use the VM main loop and `HybridMainThreadRunner`.
- `HybridMainThreadRunner::start_lifecycle` inspects manifest execution ownership. Runtime lifecycles become VM fibers; native lifecycles enter the loaded image's native scheduler.
- Native and runtime lifecycle instances are pumped from the same main-loop turn.
- The loaded native library binds and installs the generated lifecycle resolver.

Files:

- `crates/kira-hybrid-runtime/src/library.rs`
- `crates/kira-hybrid-runtime/src/session/run.rs`
- `crates/kira-llvm-backend/src/codegen/program.rs`

## Runtime ABI

Runtime ABI is now version 13:

- `RUNTIME_ABI_VERSION = 13`
- `RUNTIME_ABI_MARKER = kira_rt_abi_version_13`
- native marker function is `kira_rt_abi_version_13`

New native symbols include:

- `kira_rt_main_thread_install_lifecycle_resolver`
- `kira_main_thread_lifecycle_resolve`
- `kira_rt_main_thread_lifecycle_checkpoint`
- `kira_rt_main_thread_lifecycle_start_local`
- `kira_rt_main_thread_lifecycle_pump_local`
- `kira_rt_main_thread_lifecycle_reset_local`

`HYBRID_HOST_SYMBOLS` includes every symbol the hybrid host resolves.

Always rebuild `kira-native-bridge` before native tests after ABI changes. A stale archive fails with a missing `kira_rt_abi_version_13` marker.

## Harness and documentation

`tests-kik/main-thread-lifecycle/app/main.kira` now proves:

- Application-thread calls start two lifecycles.
- `uiLifecycle` crosses runtime/native calls and prints `42`.
- `counterLifecycle` runs beyond one slice and prints `20000`, proving preserved locals.
- A one-off `@MainThread` function coexists with both lifecycles.

Exact expected output on VM, LLVM, and hybrid:

```text
main-thread-lifecycle
42
manual-main-thread
20000
```

Tree-sitter corpus now parses a lifecycle declaration plus its start call from `@Main`.

Updated documentation:

- `sites/docs/content/docs/language-guide/execution-model.mdx`
- `sites/docs/content/docs/language-guide/concurrency.mdx`
- `sites/docs/content/docs/language-guide/annotations.mdx`
- `sites/docs/content/docs/language-reference/annotations-reference.mdx`
- `sites/docs/content/docs/language-reference/expressions.mdx`
- `sites/docs/content/docs/language-reference/execution-and-feature-status.mdx`
- `sites/docs/content/docs/appendix/diagnostics/index.mdx`

Web end-to-end coverage now expects `KSEM338` instead of the obsolete lifecycle-as-entrypoint behavior.

## Verified results

Passed:

- `cargo test -p kira-semantics main_thread --quiet`: 14 passed.
- `cargo test -p kira-vm-runtime main_thread --quiet`: 5 passed.
- `cargo test -p kira-cli --test kik_harness main_thread_lifecycle_runs_across_every_executable_backend -- --nocapture`: passed on VM, LLVM, and hybrid.
- `npm test` in `editors/tree-sitter`: 156/156 parses passed.
- `cargo check -p kira-native-bridge --target x86_64-pc-windows-msvc`: passed.
- `cargo check -p kira-native-bridge --target aarch64-unknown-linux-gnu`: passed.
- `cargo check -p kira-native-bridge --target x86_64-unknown-linux-gnu`: passed.
- Fresh workspace build with warnings as errors: passed.
- Backend parity: 433/433 passed.
- Portable VM `wasm32-unknown-unknown` check: passed.
- Formatting and file-size validator checks passed before the final VM main-loop additions.

One backend-parity validation run initially failed only this Clippy finding:

```text
initializer for `thread_local` value can be made `const`
```

Both Unix and Windows scheduler initializers were changed to `const`. Re-run Clippy is part of the active final gate below.

## Active validation

At handoff creation, this command is still running:

```bash
cargo check --workspace --all-targets && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace --all-targets --no-fail-fast
```

Observed process state at handoff creation:

```text
PID 11883: parent shell
PID 12502: cargo check --workspace --all-targets
```

The previous repository validation client timed out after five minutes while its `cargo test --workspace --no-fail-fast` child continued. That child progressed through `knvm_binstall` Apple target builds and exited, but its final exit status was not returned to the client. Do not treat it as confirmed green. Let the active explicit command finish and capture its exit status.

## Remaining work

1. Let active check, Clippy, and test chain finish. Fix every failure.
2. Split `crates/kira-vm-runtime/src/main_thread.rs` below 700 lines, then rerun affected tests and full gate.
3. Run `git diff --check`.
4. Re-run the exact lifecycle backend harness after any scheduler edit.
5. Re-run tree-sitter tests after any corpus or grammar edit.
6. Inspect `git status` carefully. Worktree contains many earlier uncommitted changes from the same larger session, including DAP, MainThread, toolchain, manifest, and generated parser work. Do not revert or separate them as unrelated user work.
7. Do not commit unless requested. No commit has been made for this feature.

## Worktree warning

The worktree is broadly dirty. Deleted tracked files such as `crates/kira-native-bridge/src/redb.rs` and `crates/kira-project/src/autobind/builtin.rs`, plus many changes outside lifecycle scheduling, predate this final continuation and belong to the larger session. Preserve them.

Generated `tests-kik/main-thread-lifecycle/.kira-build/` output may reappear after harness runs. It is build output, not source.
