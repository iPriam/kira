# AGENTS.md

## Purpose

This repository is a Zig monorepo for the Kira compiler/bootstrap toolchain. Prefer repo-specific changes over generic cleanup, and keep the package layering intact.

Kira is a dual-mode language: VM iteration and LLVM/native performance are both core promises of the project. Do not treat the LLVM backend as optional, future-only, or secondary. Any feature that works only in the VM is incomplete unless the compiler explicitly rejects it for LLVM with a tested diagnostic.


## Core Project Law: VM And LLVM Parity

Kira's promise is not VM-first development. Kira's promise is one language with fast VM iteration and native performance.

Every language, compiler, runtime, standard library, FFI, toolchain, graphics-binding, or test change must preserve or improve parity between:

- VM execution through `kira run`
- LLVM/native execution through `kira build`
- Hybrid execution when the touched feature participates in hybrid mode

If a feature works in the VM but not in LLVM, the task is not complete. The implementation must do one of the following:

1. Implement the feature correctly in LLVM.
2. Lower the feature into an LLVM-compatible representation.
3. Add a clear compiler diagnostic that rejects the unsupported feature before LLVM codegen.
4. Add tests proving the behavior is intentionally unsupported.

Silent VM-only behavior is forbidden.

Do not say:

- "LLVM can be added later."
- "Skipping native backend for now."
- "This only affects runtime mode."
- "LLVM is out of scope."
- "VM passes, so the task is done."

Instead, do one of these:

- Implement the behavior in VM and LLVM.
- Add VM and LLVM regression tests.
- Add hybrid regression tests when hybrid behavior is affected.
- Add a compiler diagnostic for unsupported native behavior.
- Add negative tests for rejected unsupported cases.
- Explain the remaining backend gap only if the current patch prevents users from accidentally depending on unsupported behavior.

A feature that cannot run native is either unfinished or must be explicitly rejected.


## Repo Shape

- `packages/` contains the compiler, runtime, build, CLI, and toolchain packages.
- `tests/` contains corpus-style integration cases plus test helpers.
- `examples/` holds runnable sample programs.
- `docs/` holds architecture, command, package graph, native library, and language-surface docs.
- `templates/` is used by `kira new`.
- `generated/`, `.zig-cache/`, and `zig-out/` are build/install outputs. Do not hand-edit them.


## Preferred Commands

Run Kira commands from the project root.

- Build the normal developer targets with `zig build`.
- `zig build` also updates the local development snapshot used by the `kira` command.
- Run the full test suite with `zig build test`.
- Use `kira` for end-to-end CLI checks after `zig build`:
  - `kira run examples/hello.kira`
  - `kira check examples/hello.kira`
  - `kira build examples/hello.kira`
- Use `zig build run -- ...` when iterating on the CLI itself because it rebuilds and runs in one step.
- Use `zig build` when validating the managed toolchain/build/dev-snapshot flow.
- When touching backend-sensitive behavior, run or add corpus coverage that exercises `vm`, `llvm`, and `hybrid` where applicable.

Build/check the repo with `zig build` and `zig build test`; use `kira ...` for user-facing CLI behavior after `zig build` has refreshed the development snapshot.


## Architecture Rules

Follow the layered graph documented in `docs/package_graph.md` and encoded in `build.zig`.

- Keep lower layers independent of higher layers. Do not add upward imports.
- Frontend pipeline changes should stay aligned with the existing flow:
  `kira_source` -> `kira_lexer` -> `kira_parser` -> `kira_semantics` -> `kira_ir`.
- Backend selection belongs in `packages/kira_build`.
- `packages/kira_cli` is the leaf command surface. Keep business logic in lower packages when possible.
- `packages/kira_main` is the app-facing C ABI facade, not a place for compiler orchestration.
- Keep `root.zig` files small and focused on exports/wiring.
- Do not add new frontend or runtime behavior without considering how it lowers to LLVM/native code.


## Where To Change Things

- Lexer/token changes: `packages/kira_lexer`, `packages/kira_syntax_model`.
- Parser/AST changes: `packages/kira_parser`, `packages/kira_syntax_model`.
- Semantic analysis and HIR lowering: `packages/kira_semantics`, `packages/kira_semantics_model`.
- Shared IR changes: `packages/kira_ir`.
- VM execution changes: `packages/kira_bytecode`, `packages/kira_vm_runtime`.
- LLVM/native changes: `packages/kira_llvm_backend`, `packages/kira_native_bridge`.
- Hybrid execution changes: `packages/kira_hybrid_runtime`, `packages/kira_hybrid_definition`.
- CLI behavior and command UX: `packages/kira_cli`.
- Toolchain/install/fetch logic: `packages/kira_toolchain`, `packages/kira_build`, `packages/kira_bootstrapper`.

When a change starts in the VM path, also inspect the LLVM/native path before finishing. When a change starts in LLVM/native code, also confirm VM behavior remains compatible.


## Definition Of Done

A compiler/runtime task is not done unless LLVM compatibility has been addressed.

For every semantic, lowering, runtime, FFI, codegen, or backend-selection change:

- Add or update VM coverage.
- Add or update LLVM/native coverage.
- Add or update hybrid coverage when hybrid behavior is affected.
- Verify that VM and LLVM agree on observable behavior.
- Add regression tests for any fixed backend mismatch.
- Add negative tests for unsupported features that must be rejected.
- Do not leave TODOs such as `LLVM later`, `native backend later`, or `VM only for now` unless the patch also adds diagnostics and tests that make the unsupported path impossible to use accidentally.

When in doubt, prefer making the compiler reject unsupported code clearly over allowing code that only works in the VM.


## Testing Expectations

Prefer targeted tests plus the repo-wide suite when practical.

- Add or update unit tests near the changed package when behavior is local.
- Add corpus cases under `tests/` for user-visible compiler/runtime behavior:
  - `tests/pass/run/` for successful execution cases
  - `tests/pass/check/` for successful analysis/check-only cases
  - `tests/fail/` for expected diagnostics
- Each corpus case should include `main.kira` and `expect.toml`.
- For runnable cases, declare the backend matrix explicitly in `expect.toml`, for example `["vm", "llvm", "hybrid"]` when all paths should agree.
- For failure cases, include the expected diagnostic code/title and stage when relevant.
- If LLVM or hybrid behavior is touched, make sure the affected corpus coverage still exercises those paths.
- A passing VM test alone is not sufficient evidence that a feature is complete.
- A passing LLVM test alone is not sufficient either.
- The expected standard is behavioral parity.

Backend-sensitive tests should be small and numerous. Prefer many focused regression tests over one giant test that hides the failure cause.


## Backend Parity Test Policy

Every feature must be tested across backends whenever possible.

Preferred coverage includes:

- Small focused regression tests for individual backend mismatches.
- Large stress tests for real-world compiler/runtime interactions.
- Negative tests for unsupported constructs.
- FFI tests for native callbacks, pointer recovery, struct passing, ownership, and lifetime behavior.
- Nested struct, descriptor, copy, and aggregate tests for VM and LLVM consistency.
- Runtime-vs-native tests for the same source program when applicable.
- Hybrid tests when VM/native boundaries, callback state, or native recovery are involved.

If a backend cannot support a feature yet, add an explicit diagnostic and a failure test. Do not leave unsupported behavior to crash, miscompile, or silently diverge.


## Docs And Examples

Keep docs and samples in sync with behavioral changes.

- Update `README.md` and `docs/commands.md` when commands, install flow, or backend behavior changes.
- Update `docs/architecture.md` or `docs/package_graph.md` when package responsibilities or dependencies move.
- Update `docs/language_inventory.md` when the implemented frontend surface or executable lowering boundary changes.
- Update `examples/` when syntax or showcased workflows change.
- If `kira new` output changes, update `templates/` and verify the generated app shape still makes sense.
- When documenting a language feature, mention backend support status if VM, LLVM, or hybrid behavior differs.
- Do not document VM-only behavior as generally supported unless LLVM/native rejection is intentional and tested.


## LLVM And Toolchain Notes

- LLVM discovery order is:
  1. `KIRA_LLVM_HOME`
  2. Kira-managed installs under `~/.kira/toolchains/llvm/...`
  3. older repo-managed fallback paths, if present
- Use `kira fetch-llvm` or `zig build fetch-llvm` to install the pinned LLVM bundle before relying on LLVM backend tests locally.
- Be careful when changing launcher, build, or toolchain behavior: `zig build` is part of the intended developer workflow and updates the local development snapshot used by `kira`.
- Do not skip LLVM validation just because the local machine is missing LLVM. Either use the managed LLVM fetch flow or add tests/diagnostics that can be validated in the intended environment.


## Agent Behavior Rules

Agents must assume LLVM compatibility is required by default.

Agents should assume Kira commands are run from the repository/project root. Do not `cd` into or depend on a `.kira/` working directory.

If the user adds instructions through a deliberate `comptime { @compileError(...) }` blocker, treat that blocker as an explicit user message and follow it exactly.

- Do not classify the blocker as corruption, an unstable worktree mutation, or a broken migration artifact.
- Do not remove the blocker just to make the build pass unless the blocker's own instructions say removal is allowed.
- If the blocker says to validate sibling projects, validate the sibling projects first, report the exact commands and results, and only then remove it.
- This rule exists because the user may not be able to send another live message while the agent is running, so `comptime @compileError` blockers are a valid way for the user to communicate mandatory instructions inside the repo.

Before finishing a task, check:

1. Does the VM path still work?
2. Does the LLVM/native path also work?
3. Does hybrid still work when relevant?
4. Are all affected paths covered by tests?
5. Are unsupported cases rejected with clear diagnostics?
6. Did the change introduce backend-specific behavior?
7. Did any new feature accidentally depend on VM-only runtime behavior?
8. Has `zig build` been run when the local `kira` development snapshot needs to reflect the latest code?

If the answer to any of these is unclear, continue the task by adding tests, diagnostics, or LLVM lowering support.

Do not stop after making the VM pass.

Do not recommend skipping LLVM support to move faster. Kira's core value is native performance with VM iteration, so VM-only progress is incomplete progress.


## Change Hygiene

- Avoid monolithic files. Split work across focused modules when a file starts accumulating multiple responsibilities.
- Treat 1000 lines as a hard upper limit for a source file, and prefer multi-file designs well before reaching it.
- This repo may be mid-refactor. Expect unrelated dirty-worktree changes and do not revert them unless explicitly asked.
- Preserve the current naming and layering style instead of introducing new abstractions casually.
- Prefer small, composable changes over cross-cutting rewrites.
- Do not commit generated artifacts from `generated/`, `.zig-cache/`, or `zig-out/` unless the task explicitly calls for them.
- Do not use generic cleanup as a reason to destabilize backend behavior.
- When refactoring, preserve existing VM/LLVM/hybrid test coverage or improve it.
