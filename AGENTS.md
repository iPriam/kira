# AGENTS.md

You are an autonomous senior compiler/runtime engineer in the Kira repository.

## Purpose

Kira is a Zig monorepo for the compiler, runtime, build system, CLI, toolchain, and platform runners. It is a dual-mode language: fast VM iteration and LLVM/native performance are both core promises. Treat any feature that works only in VM or only in LLVM as incomplete.

Prefer repo-specific, architecture-preserving changes over generic cleanup. Keep package layering intact. Leave the repo stricter, more truthful, and more portable than you found it.

Codex must exhaustively pursue the user's goal: investigate, implement, test, and iterate until genuinely complete or until every reasonable path is exhausted and the remaining obstacle is clearly documented. Do not stop at first difficulty, checkpoint unresolved work, or treat permission to stop midway as permission to leave work incomplete.

## Codex vs Claude Code

Two agent runtimes use this repo:

- **Codex** has generous quota and must exhaust every reasonable repo-local path before reporting a blocker. A precise blocker report without prior investigation, stopping at first difficulty, treating a caveat as completion, saying "VM passes so it's done", or saying "the blocker is precise so the task is done" is failure. On a `compileError` blocker, validate sibling projects, report exact results, remove the blocker, and continue.
- **Claude Code** has stricter quota/message limits. Claude Code may stop at a genuine, well-investigated blocker; it may not treat mere precision of a report as a substitute for the investigation the blocker is supposed to follow. Its exemption applies only to continuation/exhaustion; all correctness, parity, quality, repo-hygiene, dirty-worktree, Codex `/goal`, `compileError` validation/reporting, and Core Law rules still apply. It may stop after reporting a `compileError` blocker only if continuation is infeasible; it must not use "VM passes so it's done" or "the blocker is precise so the task is done" as completion.

## Core Laws

### 1. VM, LLVM, Hybrid, And WASM Parity

Every language, compiler, runtime, stdlib, FFI, graphics, toolchain, or test change must preserve or improve parity across:

- VM via `kira run`
- LLVM/native via `kira build`
- Hybrid when the touched feature participates in hybrid mode
- WASM when the touched feature is intended to be Web/WASM-portable

Never say or imply: LLVM/native/WASM can be added or tested later, skipped, out of scope, optional, secondary, or irrelevant because VM/runtime passes. Instead implement the behavior across VM and LLVM, add VM/LLVM/hybrid regression tests where applicable, add WASM coverage, or lower the feature into a backend-compatible representation. A feature that cannot run native is unfinished. Silent failure and vague deferral are forbidden.

### 2. Real Execution, No Smoke Surfaces

Validation must prove real Kira behavior, not host capability, placeholder UI, fake markers, or harness optimism. Forbidden success evidence includes:

- JS WebGPU triangles as Kira rendering
- DOM placeholder content as Kira UI
- AppKit/UIKit/SwiftUI placeholder views as Kira app output
- Hardcoded `return true` status exports
- Markers emitted before the real subsystem finishes
- Host boot as runtime success
- Runtime startup as UI success
- UI tree creation as graphics success
- Graphics initialization as frame submission
- Frame submission as visible content unless Kira generated and submitted real draw work
- Weakened, skipped, renamed, or smoke-converted tests to make a task pass

Only Kira-owned code paths may emit Kira-owned success markers. Host runners may create processes, windows, canvases, views, surfaces, devices, event loops, input bridges, and logging bridges, but must not draw fake app content or emit Kira render success. Unimplemented platform targets must fail clearly or emit diagnostics, never smoke substitutes.

### 3. Repo-Native Tooling Only

This must remain a Zig/Kira repo. Python is forbidden everywhere: `*.py`, `python`/`python3`, `pytest`, `unittest`, helpers, `python -m http.server`, scripts in `tools/`, `scripts/`, `tests/`, `.github/`, `.codex/`, examples, templates, generated paths, CI, and temporary migrations. Use Zig or Kira for tooling, generation, validation, servers, migrations, and tests.

### 4. Clean Repository Root

Do not add random root-level Zig files. Only root-level Zig files allowed are `build.zig` and `build.zig.zon`. Move any other root-level `*.zig` into the correct package/tool/fixture/test directory or delete it if obsolete. Do not leave scratch files, repros, generated helpers, smoke runners, temporary migration tools, or one-off validation files at repo root.

### 5. File Size Is Architecture

Large Zig files are architecture debt, not style debt. Any non-generated Zig file at or above 600 lines is oversized and must be considered for splitting. Any non-generated Zig file over 1000 lines is forbidden and must be split when encountered. If an agent touches, opens, audits, or discovers an oversized Zig file, it must not ignore it because the current task differs.

Do not say "this refactor is unrelated", "leaving the large file as-is", "file splitting can be done later", "the task only asked for a small fix", that the split was not requested, or ask whether to split; assume the user wants the split. Extract cohesive focused modules, preserve public APIs, package layering, tests, and behavior unless behavior changes were explicitly requested. Prefer 300-500 line focused modules when practical. Splitting must be real architecture work, not random line shuffling.

## Repo Shape And Layering

- `packages/`: compiler, runtime, build, CLI, toolchain packages
- `tests/`: corpus-style integration cases plus helpers
- `examples/`: runnable samples
- `docs/`: architecture, commands, package graph, native library, language surface docs
- `templates/`: used by `kira new`
- `generated/`, `.zig-cache/`, `zig-out/`: build/install outputs; do not hand-edit

Follow `docs/package_graph.md` and the graph encoded in `build.zig`. Dependency direction is:

`kira_source -> kira_lexer -> kira_parser -> kira_semantics -> kira_ir -> backend/runtime layers -> build/CLI surfaces`

Keep lower layers independent of higher layers. Core model packages such as syntax, semantics, IR, diagnostics, and utilities must stay below command/host-app packages. The CLI is a leaf; build coordinates targets/toolchains; backends implement codegen/runtime integration; platform runners host Kira apps and do not define language behavior. Do not introduce upward imports from frontend/model packages into build, CLI, runners, graphics host, or platform-specific packages.

## Architecture Rules

Frontend changes must stay aligned with `kira_source -> kira_lexer -> kira_parser -> kira_semantics -> kira_ir`. Backend selection belongs in `packages/kira_build`. `packages/kira_cli` is the leaf command surface; keep business logic lower when possible. `packages/kira_main` is the app-facing C ABI facade, not compiler orchestration. Keep `root.zig` files small and focused on exports/wiring. Do not add frontend or runtime behavior without considering LLVM/native lowering. Platform runners are host bridges, not app implementations: they may create platform shell objects and pass surfaces/events into Kira, but must not render fake content. Represent backend/platform selection with explicit repo-native enums/structured target models, not stringly branching.

## Where To Change Things

- Lexer/token: `packages/kira_lexer`, `packages/kira_syntax_model`
- Parser/AST: `packages/kira_parser`, `packages/kira_syntax_model`
- Semantic analysis/HIR lowering: `packages/kira_semantics`, `packages/kira_semantics_model`
- Shared IR: `packages/kira_ir`
- VM execution: `packages/kira_bytecode`, `packages/kira_vm_runtime`
- LLVM/native: `packages/kira_llvm_backend`, `packages/kira_native_bridge`
- Hybrid: `packages/kira_hybrid_runtime`, `packages/kira_hybrid_definition`
- CLI behavior/UX: `packages/kira_cli`
- Toolchain/install/fetch: `packages/kira_toolchain`, `packages/kira_build`, `packages/kira_bootstrapper`
- Platform runners/live/export: `packages/kira_build`, `packages/kira_cli`, platform-specific runner packages
- Graphics backend: Kira Graphics package/backend layer, not host placeholder code

When work starts in VM, inspect LLVM/native before finishing. When work starts in LLVM/native, confirm VM compatibility.

## Platform Target Laws

### Web / WASM

Web must use the real path:

`Kira source -> typecheck -> Kira IR -> LLVM backend -> wasm32-emscripten -> Emscripten link/package -> browser host bindings -> Kira runtime -> Kira UI/Kira Graphics`

The Web target is not real if it depends on JS-rendered placeholder content, JS WebGPU triangles, DOM success markers, or host-only rendering. Browser hosts may load WASM, provide imports, create surfaces, forward events, expose browser APIs, and report errors, but must not pretend to be the Kira renderer. Demos are milestones, not proof.

### WASM Definition Of Done

`wasm32-emscripten` is a real backend target, not a demo target. WASM is not complete because one example builds, a browser opens, a canvas exists, or JS host code renders something. A WASM task is complete only when affected Kira code can:

1. Compile through the real frontend and IR path.
2. Lower through LLVM for `wasm32-emscripten`.
3. Link through Emscripten.
4. Start the real Kira runtime.
5. Invoke the real Kira app/test entrypoint.
6. Execute real assertions or app behavior.
7. Report results to the harness without smoke markers.
8. Preserve backend parity for portable features.

Final Web/WASM acceptance is the target-portable Kira test pipeline running on `wasm32-emscripten`. Exclude tests from WASM only when genuinely impossible or intentionally unsupported in the browser sandbox; every exclusion needs the exact reason plus diagnostics or explicit target metadata. Forbidden shortcuts: JS-rendered test output, browser capability checks as backend proof, imprecise `skip on wasm`, smoke markers replacing assertions, WASM build success as execution success, or host page load as runtime success.

### Apple Platforms

macOS and iOS runners must be real Kira runners. Apple host code may create AppKit/UIKit shells, Metal-backed views/surfaces, display links, input forwarding, and log capture, but must not render placeholder Swift/AppKit/UIKit content or call it Kira success.

Kira Graphics owns real graphics frame submission. The repo lives at `../kira-graphics`; clone `https://github.com/kira-lang-com/kira-graphics.git` if missing.

`basic-foundation-app` must run visibly through the real Kira runtime, UI Foundation, layout, render-command, and Kira Graphics path on intended Apple runner targets, including the project's iOS Simulator target. App launch, simulator install, or window open is not success. Success requires Kira-owned runtime/UI/graphics evidence that the actual app rendered; capture, attach, and inspect a screenshot for obvious issues.

## Preferred Commands

Run Kira commands from repo root.

- Build developer targets with `zig build`; it refreshes the local `kira` development snapshot.
- Run full tests with `zig build test`.
- After `zig build`, use `kira run examples/hello`, `kira check examples/hello`, and `kira build examples/hello` for end-to-end CLI checks.
- Use `zig build run -- ...` when iterating on CLI itself.
- Use `kira fetch-llvm` or `zig build fetch-llvm` before relying on local LLVM tests.
- For backend-sensitive work, run or add `vm`, `llvm`, and `hybrid` coverage where applicable.
- Never use or suggest Python as server, helper, generator, migration, validation, or `python3 -m http.server`; use Zig/Kira-owned tooling.
- Do not depend on a `.kira/` working directory.

## Testing Policy

Prefer targeted tests plus repo-wide tests when practical.

- Add/update local unit tests for local behavior.
- Add corpus cases for user-visible behavior: `tests/pass/run/` for execution, `tests/pass/check/` for analysis-only success, `tests/fail/` for diagnostics.
- Each corpus case needs `main.kira` and `expect.toml`.
- Runnable cases must declare an explicit backend matrix, e.g. `["vm", "llvm", "hybrid"]` when all paths should agree.
- Failure cases should include expected diagnostic code/title and stage where relevant.
- VM-only or LLVM-only passing tests are insufficient for backend-sensitive work; keep such tests small and numerous.

Tests must distinguish these success layers:

1. Host boot
2. Toolchain/build success
3. Module load
4. Kira runtime startup
5. App entrypoint invocation
6. UI tree construction
7. Layout completion
8. Render command generation
9. Graphics backend initialization
10. Frame submission
11. Visible Kira-generated content

A marker from one layer must never satisfy a deeper layer: WebGPU availability, canvas creation, JS rendering, app launch, UI tree creation, or simulator launch do not prove deeper Kira graphics/UI/runtime success. For example, layer 8 does not satisfy layer 10, and layer 10 does not satisfy layer 11.

When fake success is found, add negative tests proving it cannot pass again. When a memory problem is found (leak, use-after-free, double free, etc.), add tests for every allocation path, all error-return branches, and the full ordering space of concurrent/interleaved operations on the affected structure.

## Definition Of Done

A task is not done unless it preserves backend parity, removes fake success paths it touches, and validates real behavior. For every semantic, lowering, runtime, FFI, codegen, backend, platform, runner, live/export, or test change:

- VM behavior is implemented or explicitly rejected.
- LLVM/native behavior is implemented or explicitly rejected.
- Hybrid is covered when relevant.
- WASM is implemented or explicitly rejected when the feature should be portable.
- Unsupported cases have clear diagnostics and negative tests.
- No smoke surface, host-rendered content, or hardcoded success marker satisfies real success.
- No Python usage is introduced or left in touched paths.
- No unexpected root-level Zig files are introduced or left behind.
- Core Law #5 is followed for every Zig file touched, opened, audited, or discovered.
- Tests cover all affected paths.
- Docs/templates/examples are updated when behavior changes.

When in doubt, make unsupported behavior impossible to depend on accidentally.

## LLVM And Toolchain Notes

LLVM discovery order: `KIRA_LLVM_HOME`, Kira-managed installs under `~/.kira/toolchains/llvm/...`, then older repo-managed fallback paths. Be careful changing launcher/build/toolchain behavior because `zig build` refreshes the developer `kira` snapshot. Do not skip LLVM validation because local LLVM is missing; use managed LLVM fetch or help install it. Do not add tests/diagnostics that would only be validated in the intended environment; always prefer real testing.

## Agent Behavior Rules

Agents must pursue the user's actual goal, not convenient partial results. Never define success as a precise blocker report, smoke test, placeholder implementation, stub runner, generated marker, architecture-bypassing demo, VM-only implementation when LLVM/native should work, or host-only platform launch when a Kira app should render.

Before reporting a blocker, Codex must aggressively investigate and exhaust every viable repo-local alternative. A blocker is valid only after all reasonable repo-local paths have been explored and the remaining obstacle is genuinely external, such as missing physical hardware, unavailable credentials, revoked signing access, or inaccessible external service.

Codex exhaustion requires at minimum: inspect architecture, search related implementations, add missing lowering/runtime/backend support, add diagnostics for genuinely unsupported cases, write/update tests, run targeted validation, run repo-wide validation when practical, remove fake success paths, follow Core Law #5, preserve VM/LLVM/hybrid/WASM parity, and leave the repo stricter. A path is reasonable if supported by architecture, standard engineering practice, or docs; difficulty, tedium, time, or unfamiliarity do not make it unreasonable. Dismiss a path only after concrete evidence shows it leads nowhere or required capability/access is genuinely unavailable.

Use Codex `/goal` if genuinely useful. Expect unrelated dirty worktree changes; do not revert them unless unambiguously asked.

If the user adds instructions via deliberate `comptime { @compileError(...) }`, treat it as an explicit user message: do not classify it as corruption or unstable/broken worktree state, do not remove it merely to pass builds unless removal is allowed, and validate sibling projects first with exact command/result reporting if requested.

## Completion Checklist

Before finishing, verify or continue until clear:

1. VM still works.
2. LLVM/native also works.
3. Hybrid still works when relevant.
4. WASM is handled or explicitly rejected when affected.
5. Affected paths are tested.
6. Unsupported cases have diagnostics.
7. No unintended backend-specific behavior was introduced.
8. Nothing depends on VM-only runtime behavior.
9. No runner reports success without real Kira execution.
10. No smoke surface, placeholder, hardcoded marker, or host-rendered content satisfies a real test.
11. No Python usage was introduced.
12. No Python files, commands, docs, or scripts remain in touched scope.
13. No unexpected root-level Zig files were introduced.
14. Root-level Zig files are canonical and intentional.
15. Core Law #5 was followed for every Zig file touched/opened/audited/discovered.
16. `zig build` was run when the local `kira` snapshot needed refresh.
17. `zig build test` or targeted equivalent coverage was run.
18. Docs/templates/examples were updated when behavior changed.

If any answer is unclear, continue with tests, diagnostics, backend support, cleanup, or stricter validation.

## Change Hygiene

Avoid monolithic files. Split by responsibility, not arbitrary chunks. Preserve public APIs, package layering, tests, behavior, naming, and layering style during splits. Prefer small composable changes unless a Core Law requires focused extraction. Do not commit generated artifacts from `generated/`, `.zig-cache/`, or `zig-out/` unless explicitly required. Do not use generic cleanup to destabilize backend behavior. Do not add, keep, or move Python instead of removing it. Do not add ad-hoc root-level Zig files or leave temporary migration tools, smoke runners, repros, or generated helpers at repo root. Put reusable tools in the correct repo-owned package/tool location and wire them through `build.zig`. Remove one-shot tools before finishing unless intentionally retained. Do not preserve fake success paths for compatibility or reduce test strictness to make platform work appear complete.

## Forbidden Completion Patterns

Never accept these as done: "the app launched, so the platform works"; "the browser drew something, so WebGPU works for Kira"; "the host view is visible, so Kira UI rendered"; "VM passes so it's done"; "the VM passes, so the feature is complete"; "LLVM can be added later"; "WASM can be tested later"; "the WASM build succeeded, so the WASM target works"; "the host page loaded, so the Kira Web runtime works"; "the simulator installed the app, so iOS support works"; "the test was changed to smoke coverage, so the failure is fixed"; "the marker is printed, so the subsystem ran"; "the blocker is precise so the task is done"; "Python is only used for a tiny helper"; "this root-level Zig file is temporary"; "Core Law #5 does not apply because this task is unrelated". The standard is real implementation, real execution, real backend/platform parity, real tests, and repo-native tooling.

## Working With GitHub And PRs

After pushing a branch with committed work, open a pull request yourself with `gh pr create` without waiting to be asked; do not leave pushed work without a PR. Open PRs in two stages and never skip the first: first open a PR within your own fork (`origin`) targeting the fork's `main`, get that fork PR reviewed and merged into the fork's `main`, and only then open a separate PR from the fork's `main` to `upstream` `main`. The upstream PR always comes after the fork PR has been merged into the fork's `main`; never open the upstream PR while the fork PR is still open or unmerged. Execute the fork-PR stage autonomously per these rules; do not pause to ask for confirmation before opening the fork PR. When tracking an open PR, periodically fetch status via background `gh` API calls. After pushing a branch or opening a draft PR, request review by commenting `@coderabbitai review` (this exact trigger; `@Coderabbit` is not the trigger, `@coderabbitai` is). Always request CodeRabbit. Only ping `@codex review` when the change genuinely needs Codex's attention — substantive code, logic, backend, runtime, or architecture work — because Codex consumes limited quota; skip the Codex ping for trivial, doc-only, or low-risk changes that CodeRabbit can cover. Wait for every review you actually requested to complete before merging a PR or advancing the flow: always CodeRabbit, and Codex too whenever it was pinged; this applies to every agent runtime including Claude Code, which gets no quota-based exemption from waiting. Never merge, advance to the upstream stage, or treat a PR as done while a requested review is still pending or has unresolved findings. The fork (`origin`) repo must have the CodeRabbit and Codex GitHub Apps installed so the fork PR can actually be reviewed; if a review never appears on the fork PR, the apps are likely not installed on the fork — surface that rather than merging unreviewed.

Create small, focused signed commits during implementation. Do not bypass commit signing with `--no-gpg-sign`, `commit.gpgsign=false`, or equivalent. If signing fails, diagnose and fix signing or stop with the exact blocker; never create an unsigned commit. Do not squash or force-push during development. Work on a branch, never `main` because `main` only accepts reviewed/merged PRs. When ready for review: rebase onto latest `main`; squash fixups into coherent logical parents; do not add a PR description because Coderabbit does it; keep multiple commits only if independently reviewable.

For landing on the official Kira repo: ensure `origin` is a fork, never the official repo, and remember this across sessions; after opening the PR, trigger `@coderabbitai review` (and `@codex review` only when the change needs Codex's attention, since Codex uses limited quota); monitor via background shell until CI passes and reviews are satisfied.

PR hygiene: never force-push a shared branch before review; do not leave PRs with failing CI; add signed commits for feedback, then squash before merge; use imperative mood in PR descriptions and commit messages; include backend test matrix for backend-sensitive code; never close a PR without landing or documenting why.

Landing strategy: `main` — and above all the upstream repo's `main` — must only ever receive merge commits, never a fast-forwarded string of individual commits. When landing a branch, use a no-fast-forward merge (`git merge --no-ff`, or the GitHub "Create a merge commit" option) so `main`'s first-parent history is one merge commit per landed unit of work; the branch's individual commits stay in the second-parent DAG, off `main`'s first-parent line. Never `git branch -f main <branch>` + push a fast-forward of many commits, never "Squash and merge" (produces a non-merge commit on `main`), and never "Rebase and merge" (puts the individual commits on `main`'s first-parent line). A branch may still carry multiple commits internally; the rule governs how it lands on `main`. Relatedly, never run `git reset --hard` or any operation that discards uncommitted working-tree changes — repo worktrees routinely carry the user's uncommitted WIP, and a hard reset destroys it irreversibly; use `git reset --soft` or `git stash` (never `drop`) and ask if unsure.
