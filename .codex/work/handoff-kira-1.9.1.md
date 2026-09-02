# Kira 1.9.1 implementation handoff

Session compaction summary, preserved verbatim for continuity.

## 1. Task

Implement full Kira 1.9.1 semantic spec (sections A-O: source text, evaluation
order, strings, numeric behavior, Any/runtime types, nominal identity, generic
compatibility/inference, traits/existentials, classes, ownership/Copy/Drop/
all-path release, NativeState, async/tasks/Seyoul, comptime/macros, derives/
Serde, FFI/ABI/target model, C layout/Web shims, hot reload/ABI versions, KIK
parity, tooling contract, migration table) in `/home/ubuntu/Dev/kira-projects/kira`,
across compiler, runtime, VM, LLVM/native, Hybrid, Web where portable,
tests-kik, Foundation, tooling (tree-sitter, formatter, LSP), docs. This is an
IMPLEMENTATION task, not an audit.

Overriding correction: **SEMICOLONS DO NOT EXIST IN KIRA** - `;` is invalid
syntax, must be refused (KLEX005), no insertion, newlines are whitespace,
statements delimited structurally; parser, tree-sitter, formatter, docs, tests
must agree.

Out of scope: Number, new Decimal, Rational, BigDecimal, UInt redesign,
literal-default changes, broad Int replacement.

Rules: repair existing architecture, never redesign Kira around Rust, inspect
existing implementations first (`distinct` already exists), work in
dependency order (16 steps), never convert error to Void, never weaken tests,
reject unsupported backend behavior before execution, don't claim Hybrid
without a real boundary, build + focused tests + affected tests-kik after
each subsystem, fix own regressions, record pre-existing failures with
evidence, final cross-system sweep, broadest practical suite, then a concise
completion report (subsystems changed, architectural changes, user-visible
breaking changes, tests/backends executed, remaining failures pre-existing
vs unresolved).

Repository rules in force (AGENTS.md/CLAUDE.md): omit `Co-Authored-By` and AI
trailers; commits must be verified/signed and from the machine's email; do
not push/PR/merge unless asked; read `working-with-git` skill before any git
command except diff/status; scratch only in `.codex/tmp/`, durable notes in
`.codex/work/`; never write under `.claude/`; no Python in tracked files;
`.rs` files split at 700 lines and never >=1000, `.kira` files under 700;
docs only in `sites/docs`; tree-sitter updated with syntax changes; every
syntax/behavior change needs `tests-kik` coverage; spell top type is `Any`;
verify before claiming completion; fix pre-existing issues too; prefer
mature-compiler long-term decisions over small patches.

Session style: caveman (terse) mode active for chat responses; code/comments/
docs/commits stay normal prose.

## 2. Architecture reference

Pipeline: `kira_lexer` -> `kira_parser` AST -> `kira-semantics`
(Analyzer/typeck/ownership) -> HIR (`kira-semantics-model`) -> `kira-ir`
(lower, mid/scope releases, erase) -> `kira-bytecode` -> `kira-vm-runtime`;
`kira-llvm-backend`; `kira-native-bridge` (`kira_rt_*` C ABI);
`kira-hybrid-runtime`; `kira-main` (foreign sessions); `kira-runtime-abi`
(HostCapabilities, NativeStateStore, TaskExecutor).

Environment: 2 CPUs, 1.8GB RAM. `export PATH=$HOME/.cargo/bin:$PATH`;
`KIRA_FOUNDATION_HOME=$PWD/foundation`. VM harness run takes ~6 min.
`target/debug/libkira_native_bridge.a` is NOT rebuilt by
`cargo build -p kira-cli` - after any runtime-abi/native-bridge change, run
`cargo build -p kira-native-bridge` explicitly before relying on native/LLVM
results.

Skills to read before touching related code: `debugging-programs`,
`verifying-work`, `where-to-change`, `wire-formats`, `working-on-macos`,
`working-with-agents-instructions`, `working-with-git`, `working-with-markdown`,
`working-with-workflows`, `writing-rust`.

## 3. 16-step dependency order and status

1. Source text / lexer / semicolon refusal - COMPLETE
2. Evaluation order - COMPLETE
3. Strings - PARTIAL
4. Numeric behavior / ownership HIR nodes, must-release analysis, partial
   init/moves, hidden-local ownership - PARTIAL, needs more work
5. Any/runtime types / existential writeback checks in hybrid - PARTIAL
6. Nominal identity / `.type` descriptor, Task/Cell/NativeState into Any,
   attempt-integrated cast failure/TypeCastError, box Drop metadata - PARTIAL
7. Generic compatibility/inference / NativeState refcount model - **DONE**
   (this is the slice actually completed this session; see section 4)
8. Traits/existentials / async/tasks/Seyoul - SURVEY STARTED, not implemented
9. Classes / (paired with 8 in original numbering, see pending) - NOT STARTED
10. Ownership/Copy/Drop/all-path release - overlaps step 4, NOT STARTED beyond
    survey
11. comptime/macros (macro visibility/hygiene/reflection) - NOT STARTED
12. derives/Serde grammar - NOT STARTED
13. FFI/ABI/target model (Bool ABI, RawPtr.null, FFI validation) - NOT STARTED
14. C layout/Web shims (C layout assertions/Web shims) - NOT STARTED
15. Hot reload/ABI versions - NOT STARTED
16. KIK parity / tooling contract (tree-sitter/formatter/LSP/docs) / migration
    table / audit defects incl. diagnostics registry generation (~275 of
    ~370 codes) - NOT STARTED

Note: numbering above is approximate reconstruction from the original spec
sections A-O mapped onto the working 16-step list; treat the letter-section
list (A-O) in the original task prompt as ground truth for scope, this
numbered list as a rough todo tracker.

Generic inference rewrite and removal of enum-instantiation widening: not
started, still pending somewhere in steps 7-9 territory conceptually but not
begun.

## 4. Slice 7 - NativeState refcount model (COMPLETE, verified this session)

### Design

Store `Entry.refs` (create=1). New ops: `retain`, `release(-> bool
destroyed)`, `live()`, `owners()`. Box header gets `refs`. A handle owns one
ref and releases it with scope (drop glue) since `owns_heap(NativeState) =
true`. Reads of NativeState locals **take** (new
`TypeTable::takes_on_read(ty) = NativeState || runs_user_drop`).

`nativeUserData(state)` on a local = non-taking `LoadLocal` +
`NativeUserData { shared: true }` (retains, token owns one ref); on a
temporary = `{ shared: false }` (temp's ref becomes the token's).

`nativeUserDataRetain(token)` / `nativeUserDataRelease(token)`: RawPtr only,
KSEM361 if not.

`nativeStateFree(x)` is now deprecated: does one release, warns KSEM360,
marks handle moved.

KSEM116 removed (registry too). KSEM117 kept only for closure captures.
`handed_out` ownership machinery removed entirely. Tokens never reused ->
stale tokens trap `UnknownToken`.

VM heap queues `native_state_retains/releases` on `copy_value`/
`drop_value(Value::NativeState)`, settled in interp loop via
`settle_native_state()` (retains applied before releases).

LLVM: `release_at_walk`/`retain_at_walk` NativeState arms call
`kira_rt_native_state_release/retain`; live flags/take logic now keyed by
`takes_on_read`.

New warning machinery: `Analyzer::emit_warning` (Severity::Warning) in
`analyze/field.rs`; `kira_diagnostics::has_errors` gates builds so warnings
don't fail compiles.

### Bytecode/wire changes

`NATIVE_STATE_FREE` renamed `NATIVE_STATE_RELEASE` (0x4c),
`NATIVE_STATE_RETAIN` added (0x89). `NativeUserData { shared: bool }` encoded
as opcode + bool byte (decoded in `next_instruction`, not the operandless
table - `self` isn't available there, see error notes below).

### Native symbols

`kira_rt_native_state_retain`, `kira_rt_native_state_release`,
`kira_rt_native_state_free` (now an alias of release),
`kira_rt_native_state_box_retain`. Hybrid binds STATE_RETAIN/STATE_RELEASE.
runtime-abi symbol list updated.

### Task executor survey (context carried into slice 8, not yet acted on)

`TaskPrim` wire bytes 0-11 (append-only). `TaskExecutor` uses 1-based
handles, no generations/reclamation, `pick_ready` oldest-first, virtual
clock. Scheduler policy is synthesized IR (`kira-ir/src/tasks.rs`: `TaskFns`
SPAWN/STEP/AWAIT/DETACH/CANCEL/YIELD/SLEEP, `drive_then`, nested drives on
stack). Native `kira_rt_task_op` and all native traps currently call
`std::process::exit(1)` (runtime.rs lines 452, 464, 506, 523, 555, 630;
traps.rs:25) - this is a defect to fix in slice 8, see section 6.

### Files touched (slice 7)

Docs:
- `sites/docs/content/docs/language-guide/interop-and-ffi.mdx`: fixed
  `retains: <parameter>;` -> no semicolon, `sappRun(...): Void` -> `-> Void`,
  `{ layout: c; }` -> `{ layout: c }`; rewrote "Opaque Callback State"
  section (table of 5 intrinsics, refcount rules, example, deprecation
  note, "What Foreign Code May Keep" subsection).

Runtime-abi:
- `crates/kira-runtime-abi/src/native_state.rs`: `Entry { ty, value, refs: u64
  }`; `create` sets refs=1; added `retain`, `release -> Result<bool>`,
  `live()`, `owners()`; removed `free`.
- `crates/kira-runtime-abi/src/native_state/tests.rs`: `free` -> `release`,
  new test `a_state_is_destroyed_by_the_release_that_drops_its_last_owner`.
- `crates/kira-runtime-abi/src/lib.rs`: `HostCapabilities.native_state_free`
  replaced by `native_state_retain` + `native_state_release` (default
  NoStateHost); symbol list gained retain/release names.
- `crates/kira-runtime-abi/src/native_state/host.rs`: impls delegate to
  store; added `pub fn store(&self) -> &NativeStateStore`.

Host wiring:
- `crates/kira-runtime-abi/src/file_system/host.rs`
- `crates/kira-vm-runtime/src/main_thread/hosts.rs`
- `crates/kira-main/src/callback.rs`
- `crates/kira-main/src/foreign.rs`
- `crates/kira-hybrid-runtime/src/session/host.rs`
- `crates/kira-hybrid-runtime/src/library/native_state.rs`
- `crates/kira-hybrid-runtime/src/library.rs` (`StateCountFn`,
  `state_retain`/`state_release` bound to
  `kira_rt_native_state_retain/release`)

Native bridge:
- `crates/kira-native-bridge/src/state_box.rs`: `BoxHeader.refs`,
  `kira_rt_native_state_box_retain`, box_free decrements and frees at zero;
  new test `a_retained_box_is_freed_by_its_last_release`.
- `crates/kira-native-bridge/src/native_state.rs`:
  `kira_rt_native_state_retain`, `kira_rt_native_state_release`,
  `kira_rt_native_state_free` alias.

Bytecode:
- `crates/kira-bytecode/src/op.rs`, `op/opcode.rs`, `op/codec.rs`,
  `op_tests.rs`: instruction/opcode changes above.
- `crates/kira-bytecode/src/compile/expression.rs`: NativeUserData compile
  (shared when `IrExpr::Local(slot)` and `local_is_taken(slot)`),
  NativeStateRetain/Release; take predicate now `takes_on_read`.
- `crates/kira-bytecode/src/compile/function.rs`: `local_is_taken` uses
  `takes_on_read`.

Semantics model:
- `crates/kira-semantics-model/src/ty/table/mod.rs`: `owns_heap` includes
  `Type::NativeState(_)`; new `takes_on_read`.

VM runtime:
- `crates/kira-vm-runtime/src/interp/native_state.rs`:
  `native_user_data(shared)`, `native_state_retain`, `native_state_release`,
  `settle_native_state`, `pop_state_token`.
- `crates/kira-vm-runtime/src/interp/instructions.rs`: dispatch.
- `crates/kira-vm-runtime/src/interp/frames.rs`: `is_heap_value` includes
  NativeState.
- `crates/kira-vm-runtime/src/interp.rs`: loop settles native state after
  pending drops.
- `crates/kira-vm-runtime/src/value/mod.rs`: heap fields
  `native_state_retains/releases`, `owes_native_state`,
  `take_native_state_events`.
- `crates/kira-vm-runtime/src/value/object.rs`: copy/drop arms.
- `crates/kira-vm-runtime/src/error.rs`: `NativeStateOperation::{Retain,
  Release}` (Free removed).
- `crates/kira-vm-runtime/src/native_state_tests.rs`: mechanical migration
  (`NativeUserData { shared: false }` after LoadLocal/temp, `TakeLocal +
  NativeStateRelease + Pop`), double-free test rewritten via token, new
  tests `owners_are_counted_and_the_last_release_destroys_the_state`,
  `a_handle_dropped_with_its_frame_releases_one_owner`.

IR/HIR layers (`NativeStateFree` -> `NativeStateRelease` + new
`NativeStateRetain`):
- `crates/kira-semantics-model/src/hir/exprs.rs`
- `crates/kira-ir/src/ir/exprs.rs`, `ir.rs`, `lower.rs`, `erase.rs`,
  `mid/scope.rs`
- `crates/kira-llvm-backend/src/reachability.rs`
- `crates/kira-build/src/callgraph.rs`
- `crates/kira-semantics/src/constant_order.rs`

LLVM backend:
- `crates/kira-llvm-backend/src/codegen/native_state.rs`:
  `lower_native_user_data` (Local non-view: load slot i64 without take, call
  retain, check status), `lower_native_state_retain`,
  `lower_native_state_release`.
- `crates/kira-llvm-backend/src/lower/expr/core.rs`: dispatch.
- `crates/kira-llvm-backend/src/types/runtime.rs` +
  `runtime_declare.rs`: `native_state_retain/release` callables.
- `crates/kira-llvm-backend/src/values/release.rs` +
  `values/copy.rs`: NativeState arms.
- `crates/kira-llvm-backend/src/lower/expr/storage.rs` and `program.rs`:
  predicates now use `takes_on_read`.

Semantics analysis:
- `crates/kira-semantics/src/typeck/native_state.rs`: `Counting` enum,
  `analyze_native_state_count` (KSEM361), `analyze_native_state_free`
  (KSEM219 check, KSEM360 warning, mark moved -> `NativeStateRelease`),
  `nativeUserData` no longer marks handed out.
- `crates/kira-semantics/src/analyze/field.rs`: `emit` -> `emit_with_severity`,
  new `emit_warning`.
- `crates/kira-semantics/src/ownership.rs`: removed `handed_out` field,
  `check_native_state_overwrite`, `check_native_state_handles`.
- `crates/kira-semantics/src/analyze/scope.rs`: removed `mark_handed_out`,
  `unfreed_native_state_handles`.
- `crates/kira-semantics/src/analyze/function.rs`, `place.rs`: call sites
  for the above removed.

Tests:
- `crates/kira-semantics/src/tests/native_state.rs`: STATE uses
  `nativeUserDataRelease(token)`; new tests
  (`owner_count_intrinsics_take_a_raw_pointer_token` KSEM361 x2,
  arity KSEM220/KSEM221, `a_handle_releases_its_reference_when_its_scope_ends`,
  `native_state_free_is_a_deprecated_release` (KSEM360 warning severity),
  `a_token_may_leave_the_body_that_exported_it`,
  `a_handle_may_be_exported_and_still_used`,
  `reading_a_handle_after_releasing_it_is_use_after_move` (["KSEM360",
  "KSEM107"]), `a_handle_type_is_inferred_rather_than_written` KSEM050,
  `overwriting_a_live_handle_releases_the_old_one`,
  `moving_a_handle_transfers_its_owner` (["KSEM050", "KSEM107"])). All 823
  semantics tests pass.
- `crates/kira-semantics/src/tests/closures.rs`, `tests/drop.rs`:
  nativeStateFree uses removed.
- `crates/kira-cli/tests/backend_parity/native_state.rs`: all tests migrated
  to token locals + `nativeUserDataRelease`; new
  `native_state_owners_are_counted_on_every_backend` (heap balance, expects
  "2\n7\n1\n") and `a_token_past_its_last_release_traps`
  (assert_trap_parity, "4\n").
- `crates/kira-cli/tests/backend_parity/closures.rs`: uses
  `nativeUserDataRelease(handle)`.
- `crates/kira-cli/tests/backend_parity/any.rs`:
  `a_cast_to_the_wrong_type_traps` now `let point = boxed as Point;
  print(point.x)` (was printing struct directly, hit KSEM081).
- `crates/kira-cli/tests/backend_parity/seam.rs`: nested_any test builds
  `envelope()` twice instead of `copy value` (struct with `Any` member
  refused by KSEM356 per spec, `copy` not valid there).
- Fixtures `crates/kira-cli/tests/fixtures/live/native_state.kira`,
  `fixtures/ffi/ffi_program_state_callback.kira`:
  `nativeUserDataRelease(handle)`.

tests-kik:
- `tests-kik/harness/app/NsxNativeStateRefTests.kira` (new, 7 constructs:
  token outlives handle=12, retain pairs=4, two exports=8, loop scope
  release=1275, moved handle=21, overwrite=22, deprecated free=6).
- `tests-kik/harness/app/OwyTests.kira`: adds `nativeUserDataRelease(handle)`.
- `tests-kik/ffi-harness/app/adversarial/FaxSeamStressTests.kira`: two sites
  -> `nativeUserDataRelease(handle)`.
- `tests-kik/ffi-harness/app/adversarial/FrxStateOwnerTests.kira` (new, 2
  constructs: 4141, 22).

Foundation/docs:
- `foundation/app/Kira/Diagnostics.kira`, `DiagnosticCodes.kira`: KSEM116
  removed, KSEM360/KSEM361 added.
- `sites/docs/content/docs/appendix/diagnostics/index.mdx`: rows for
  KSEM360/361.
- `sites/docs/content/docs/language-reference/expressions.mdx`: intrinsic
  list updated.
- `sites/docs/content/docs/appendix/ffi-workflows/callbacks.mdx`: mentions
  the new model.

Scratch (in `.codex/tmp/`, disposable):
- `nsref/main.kira` (retain/release program; VM and LLVM both print `2` then
  trap on stale token)
- `anycast/main.kira`
- `chain2.sh` / `chain2.log` (background verification chain)

### Errors hit and fixes (slice 7)

- `codec.rs` E0424 `self` in operandless decode table: moved
  `NATIVE_USER_DATA` decode (with `read_bool`) into `next_instruction`
  beside `NATIVE_RECOVER` - operandless-table closures don't have `self`.
- 5 semantics tests failed after the change (still calling `nativeStateFree`
  expecting no diagnostics): removed those calls / switched to
  `nativeUserDataRelease`.
- Parity `a_cast_to_the_wrong_type_traps`: KSEM081 `print` cannot format a
  struct -> changed to print `point.x`.
- Parity `nested_any_crosses_both_seam_directions_and_releases`: `copy value`
  of a struct with an `Any` member is refused by KSEM356 per spec ->
  construct the value twice via `envelope()` instead.
- Stale `libkira_native_bridge.a` causes native/LLVM tests to silently run
  against old code - always `cargo build -p kira-native-bridge` after
  runtime-abi/native-bridge changes, before trusting LLVM/native results.

### Verification status (as of this handoff)

Unit tests green: runtime-abi 115, native-bridge 114, bytecode 157,
semantics 823, all passing standalone.

A background verification chain (`chain2.sh` -> `chain2.log`) was run
covering: backend_parity, e2e, unit tests across kira-llvm-backend/hybrid/
main/build/ir/cli, ffi-harness on vm/llvm/hybrid (expect 276 each), and the
VM/LLVM harness (expect 1427 each). Result of that run:

- `== parity`: 72 passed, **5 failed** (native_state tests: `a_token_past_
  its_last_release_traps`, `callback_state_enum_preserves_raw_pointer_and_
  capture_cell_payloads`, `callback_state_mutation_crosses_runtime_and_
  native_byte_identically`, `callback_state_shares_a_capture_cell_rather_
  than_copying_it`, `native_state_owners_are_counted_on_every_backend`)
- `== e2e`: 109 passed, **8 failed** (exports::the_web_builds_an_exported_
  library, ffi_wasm::* x4, installed_toolchain::a_compiler_outside_any_
  toolchain_resolves_through_the_selection, packages::a_library_builds_for_
  the_web, web::a_web_build_links_and_runs_with_the_vms_exact_output) -
  these look like pre-existing wasm/toolchain infra failures, unrelated to
  native state; NOT YET CONFIRMED as pre-existing, needs a check on a clean
  baseline before/without this session's changes.
- `== units`: all green (83, 17, 22, 72, 38 passed across the listed crates).
- `== ffi vm` / `== ffi llvm` / `== ffi hybrid`: each 276 passed, 0 failed.
- `== vm harness` / `== llvm harness`: each 1427 passed, 0 failed.

**Re-run after the chain completed**: the 5 parity `native_state::*`
failures from the log did NOT reproduce when the same tests were re-run
directly (`cargo test -p kira-cli --test backend_parity native_state::`):
all 11 native_state parity tests passed cleanly. This strongly suggests the
chain2.log failures were caused by a stale `libkira_native_bridge.a` (not
rebuilt at the time the chain started) or a parallel-execution race across
the test binaries sharing temp/output dirs, not a real code defect.

**Still needed before calling slice 7 fully verified**: re-run the full
`backend_parity` suite (not just the `native_state::` filter) under normal
parallelism to make sure nothing else regressed and the pass was not a
fluke; confirm the 8 e2e failures are pre-existing (check whether they fail
on a git stash / clean checkout, or find prior evidence in earlier session
notes) rather than caused by this session's edits - they look infra-related
(wasm/emscripten/toolchain) but this has not been explicitly confirmed.

## 5. Progress log file

`.codex/work/kira-1.9.1-progress.md` was supposed to be updated with slice 7
completion and design notes - **this update was still pending** when this
handoff was written. Check whether that file exists and reconcile/append
this handoff's section 4 into it.

## 6. Slice 8/9 - async/tasks/Seyoul (SURVEY ONLY, not implemented)

### Required design (from spec, not yet built)

- Generation-tagged task handles with row reclamation: handle word =
  `(generation << 32) | (row+1)`.
- Rows reclaimed on `TakeResult` (joined) or `MarkDetached`-after-complete.
- Free list with generation bump on reclaim.
- Stale handle access -> new `TaskTrap::StaleHandle` (join consumes/
  reclaims, detach reclaims after completion, cancellation requests then
  reclaims, reuse only after generation advance).
- Cancellation observed at yield/await/sleep boundaries.
- Timers ordered by deadline then sequence.
- First-failure semantics with cleanup as secondary diagnostics.
- Task table owned by the runtime session (NOT per-Interp/per-thread reset
  as it is today).
- Drop cleanup runs on completion/failure/cancellation.
- Remove `std::process::exit` from native bridges entirely - replace with a
  structured runtime failure path; CLI maps that to exit code 1; hybrid/
  embedders get a structured error value instead of the process dying.

### Survey already done (files read, not yet changed)

- `crates/kira-runtime-abi/src/tasks.rs` (full) - `TaskExecutor`, 1-based
  handles, no generations/reclamation, `pick_ready` oldest-first, virtual
  clock.
- `crates/kira-ir/src/tasks.rs` (header, `build_await/detach/drive_then/
  cancel/yield/sleep`) - scheduler policy synthesized as IR, `TaskFns`
  SPAWN/STEP/AWAIT/DETACH/CANCEL/YIELD/SLEEP, nested drives on a stack.
- `crates/kira-native-bridge/src/tasks.rs` - thread-local `TASKS`,
  `kira_rt_task_reset`, `kira_rt_task_op` exits process on trap (defect).
- `crates/kira-native-bridge/src/traps.rs` and `runtime.rs` exit sites:
  lines 452, 464, 506, 523, 555, 630 in runtime.rs; traps.rs:25 - all call
  `std::process::exit(1)`, all need replacing.
- `crates/kira-semantics/src/tasks.rs` lines 230-330 -
  `analyze_task_property` (`.await`), `analyze_task_method`
  (`.requestCancel()`/`.detach()`), KSEM159 slot restriction.
- Spec sections E (async/task/Seyoul) and F (comptime/macros) were
  extracted from the original transcript for reference but are not
  reproduced in this handoff - re-pull from the task prompt / original
  spec doc if needed, or ask the user for the spec doc location.

### Still to read/design before writing code (not yet done)

- VM `TaskOp` handling in `crates/kira-vm-runtime/src/interp.rs` /
  `instructions.rs`.
- Hybrid `crates/kira-hybrid-runtime/src/session/run.rs` task lines.
- `sites/docs/content/docs/language-guide/concurrency.mdx`.
- `crates/kira-cli/tests/backend_parity/tasks.rs`.
- LLVM `crates/kira-llvm-backend/src/codegen/entry.rs` task reset.
- How the CLI currently maps VM errors to exit codes (needed to know what
  the structured-failure replacement for `process::exit` should plug into).

## 7. Immediate next steps (in order)

1. Re-run full `cargo test -p kira-cli --test backend_parity` (all tests,
   not just native_state filter) to make sure the chain2.log failures were
   really just staleness/races and not a real regression. Do this with
   `cargo build -p kira-native-bridge` run fresh first.
2. Determine whether the 8 e2e failures (web/wasm/toolchain-related) are
   pre-existing by checking whether they're independent of this session's
   changes (git blame / stash test, or look for prior mention in earlier
   session notes/progress file).
3. Write/update `.codex/work/kira-1.9.1-progress.md` with slice 7 done,
   verification evidence, and open questions above.
4. Begin slice 8 (tasks/Seyoul) design: generation-tagged handles, replace
   `process::exit` call sites, task table ownership move to runtime session,
   cancellation-at-boundary semantics - using the reads already completed
   plus the remaining reads listed in section 6.
5. Continue down remaining slices in section 3 in order, each with build +
   focused tests + affected tests-kik, fixing own regressions, recording
   pre-existing failures with evidence.
6. Final cross-system sweep, broadest practical suite, then write the
   completion report (subsystems changed, architectural changes, user-
   visible breaking changes, tests/backends executed, remaining failures
   pre-existing vs unresolved).

## 8. Rules to keep re-applying every slice

- No semicolons anywhere in Kira source (KLEX005), including in docs/
  tests-kik/tree-sitter fixtures.
- `.rs` files: split at 700 lines, hard cap under 1000. `.kira` files:
  under 700.
- Docs only under `sites/docs`.
- No Python in tracked files.
- No `Co-Authored-By` or AI trailers in commits; commits verified/signed
  from the machine's email; no push/PR/merge unless user asks.
- Read `working-with-git` skill before any git command beyond diff/status.
- Scratch in `.codex/tmp/`, durable notes in `.codex/work/`, never write
  under `.claude/`.
- Every new syntax/behavior needs `tests-kik` coverage.
- Rebuild `kira-native-bridge` explicitly after runtime/native-bridge edits
  - `cargo build -p kira-cli` will NOT pick it up automatically.
