# Kira 1.9.1 implementation progress

Spec: Kira 1.9.1 semantic specification (session 2026-09-01). Correction: semicolons do not
exist in Kira; `;` is invalid syntax, statements are delimited structurally.

Baseline before work: `cargo check --workspace` clean; `kira test --backend vm tests-kik/harness`
= 1381 passed (5m28s on this 2-core host).

## Slices

1. grammar: BOM, nested block comments, fatal escapes, `;` removed, required commas, evaluation order

### Slice 1 status (2026-09-01)
- Lexer: leading BOM skipped, misplaced BOM KLEX006, nested block comments (KLEX004 unterminated),
  `;` is KLEX005 (TokenKind::Unknown), unknown escapes fatal (`decode_string_literal` fallible).
- Parser: `skip_unknown`/`eat_separator`; required commas in array/struct literals (KPAR002);
  FFI/Export annotation blocks comma-separated; bodyless functions structural; KPAR011 reworded.
- All `.kira` sources, Rust-embedded Kira, docs, tree-sitter grammar+corpus migrated
  (tools in `.codex/tmp/desemi*.py`, `addcommas.py`).
- Evaluation order: struct literal fields evaluate as written (`FieldOrder` in HIR/IR,
  VM `NewStructOrdered`, LLVM slot insertion). Tests: EvxEvaluationOrderTests, LxxLexicalTests.
- Numeric widths: `IntWidth` (runtime-abi); checked arithmetic (VM *Checked opcodes 0x78..0x84,
  LLVM overflow intrinsics + `kira_rt_trap_{overflow,shift,narrow,float_to_int}`); narrow range
  checks; checked spelling conversions (`coerce_into` → `Convert IntToInt`); literal range KSEM350;
  shift-count traps; float→int traps; `wrappingAdd/Sub/Mul` builtins; `charAt` returns `U8`.
- Harness tally: 1395 (before Nwx tests).
- Strings: file text reads keep NUL bytes and refuse non-UTF-8 (empty, same as unreadable);
  C strings crossing into Kira validate UTF-8 (`ForeignAdapterStatus::INVALID_UTF8`, hybrid
  callback `fatal`); VM libffi already refused (`InvalidCStringResult`).
- Numeric (cont.): `Int`/width operands do not mix unless one is a fitting literal (KSEM071);
  `U64` prints/formats unsigned (`PrintUnsigned`/`StringOfUnsigned`, `kira_rt_print_uint`,
  `kira_rt_str_of_uint`); struct fields and array elements release in reverse order on VM,
  LLVM, native. Harness tests: NwxNumericWidthTests; parity: widths.rs (merged), arithmetic.rs,
  bitwise.rs shift trap.
- Audit defect noted for slice 16: `foundation/app/Kira/Diagnostics.kira` (`KiraError`) lists 275
  codes while the compiler emits ~370 distinct ones (KPAR/KSEM/KMAC gaps); the registry claims to
  name every code. Plan: generate the enum + `kiraErrorFromCode` from the compiler's code table.

### Slice 2 status (2026-09-02)
- `PackageIdentity { name, version, instance }` + `NominalIdentity`/`NominalKind` in
  kira-semantics-model (`ty/identity.rs`); `TypeTable::{declare_package, package, identity,
  identity_key}` (`ty/table/identity.rs`); struct/enum/distinct tables record declaring module;
  `DistinctTable` owner-keyed; `StructTable::record_instantiation`.
- Module identities carry `name@version#instance::Module` (program-graph `PackageRoot::resolved`,
  `ImportTable::resolved_package_module_identity`, `parse_package_identity`, `module_of`).
- Generic enum instantiation owner-keyed; `Instantiation.template` package-qualified; function
  symbols `Pkg::name` (+ identity-keyed overload suffixes); native-state fingerprints mix
  `identity_key`. Distinct headers owner-keyed with visibility resolution.
- Tests: end_to_end/packages.rs `same_named_declarations_in_two_packages_are_different_types…`.
- Known: `target/debug/libkira_native_bridge.a` is not rebuilt by `cargo build -p kira-cli`;
  single-file LLVM runs link it → run `cargo build -p kira-native-bridge` after runtime changes.
- Slice 2 (cont.): generic template tables (`generic_enums/aggregates/functions`), trait table, and
  distinct headers are owner-keyed (`template_key`, `visible_*_key`); member namespace qualified by
  owner (`member_owner_name`: `Pkg::Point.method`, `Pkg::Point$init`, specialization suffixes);
  trait existential rows displayed bare but filed per package. e2e identity test covers struct,
  enum, distinct, class, generic enum, trait existential, Any equality on vm/llvm/hybrid.
  Pending: construct families and aliases still bare-keyed.
- Slice 2 (cont.): construct families and aliases owner-keyed (`visible_family_key`,
  `visible_alias_key`, alias bodies resolve in their declaring file); implicit self-method,
  parent-call, and conformance lookups qualified. Generic templates now need the declaring
  package imported like every other declaration (ten harness files gained `import Foundation`).
  Lazy constant analysis restores the current file (was leaking the constant's file).

### Slice 3/5 status (2026-09-02)
- `CallableSignature` (kira-semantics-model hir/callable.rs): receiver+mutability, per-param
  label/type/ownership/default, result, async, ThreadAffinity, Execution. Carried on `FuncSig`
  and `HirFunction` (synthesized functions get `CallableSignature::synthesized/dispatcher`).
- Trait conformance compares the full contract (`contract_differences` → KSEM293 listing
  ownership/label/default/receiver/async/affinity mismatches) beyond the type match.
- Tasks: `Task { f(x) }` resolves overloads like a call; target must be `async` (KSEM352);
  borrowed parameters refused (KSEM353); direct calls of async functions refused (KSEM354).
  Harness/parity/semantics tests migrated (AsyncSpine*, backend_parity/tasks.rs, tests/tasks.rs,
  tests/markers.rs); concurrency docs updated.
- Slice 4 (partial): `copy value` needs Copyable proof (`copy_refusal`: native state, cell, task
  handle, Any, C storage, Drop-reaching structs/enums, arrays thereof → KSEM356); `copy`
  parameters checked at declaration; `HirExpr::Copy` node (IR lowers to the value read); `Cell`
  no longer leaf-copyable. Class specialization cap now KSEM357 instead of silent parent fallback.
  Harness: OwzCopyTests.kira (tally 1414).
- Slice 6 (partial): `value is Type` / `value as Type` (keyword `is`; parser bp 8 between
  comparison and shift; HIR `TypeTest`/`TypeCast`; IR; VM `TypeTest`/`Downcast` opcodes 0x87/0x88
  with `VmError::TypeCastFailed`; LLVM tag compare + `kira_rt_trap_cast`; tree-sitter
  `type_test_expression`/`type_cast_expression`). KSEM358 (operand not Any), KSEM359 (target not
  erasable). Harness AnxTypeTests (tally 1420), parity any.rs, semantics/parser tests, docs.
   Remaining for slice 6: `value.type` runtime descriptor, Task/Cell/NativeState erasure, cast
   failure through `attempt` (TypeCastError), Drop metadata carried by the box.

### Slice 7 status (2026-09-02)
- Native callback state has counted owners across VM, LLVM/native, and hybrid: handles, exported
  userdata tokens, and explicit retains. `nativeUserDataRetain` and `nativeUserDataRelease` are
  language intrinsics; `nativeStateFree` is a deprecated one-release alias (KSEM360/KSEM361).
- Native boxed state and backend-neutral state trees reclaim on the last release. Tokens are never
  reused. VM capture-cell releases are drained after final state settlement as well as between
  instructions.
- Harness: `NsxNativeStateRefTests.kira` and `FrxStateOwnerTests.kira`; tally 1427 VM/LLVM and 276
  ffi-harness VM/LLVM/hybrid. Focused native-state parity: 11 passed.

### Slice 8 status (2026-09-02)
- Task targets are async-only, direct async calls are refused, overload resolution is shared with
  calls, and task parameters cannot borrow (KSEM352/KSEM353/KSEM354).
- Task-table handles now include a slot generation. Join, detach, and pending cancellation reclaim
  their slot; reused storage cannot make a stale handle name a new task. FIFO spawn-order selection
  remains shared by VM and native through `TaskExecutor`.
- Focused task parity: 14 passed. Remaining: resumable owned frames, suspension across lifecycle and
  hybrid transitions, structured first-failure propagation, and native bridge failure returns.

### Slice 4 status: specialization widening removed (2026-09-02)
- One generic specialization no longer reaches another. `Result<Int, E>` in a `Result<Any, E>`
  position is `KSEM032`, in every position that admits a declared type. A program rebuilds the
  value instead, erasing each payload where it writes it.
- Deleted end to end: `TypeTable::admits`/`widens_to` and `ty/widening.rs`, `HirExpr::Widen`,
  `IrExpr::Widen`, the bytecode compiler's synthesized widen helpers (`compile/widen.rs`, four
  `CompileError` variants), the LLVM `codegen/widening.rs` leaves and `widen_leaves` cache, and the
  `kira.widen.` profile prefix. `Analyzer::admits` is now `Type::assignable_to`.
- The hybrid manifest's `internal_functions` field stays: it counts whatever the bytecode half
  appends, and is no longer worded around widen helpers. No wire bytes changed.
- tests-kik: every `Result<X, TestFailure>` in the three harnesses is now `Result<Any, TestFailure>`
  (1744 sites), which is what a case always meant; new `SpxSpecializationTests.kira` proves the
  rebuild carries payload identity, nests, and keeps the failure variant. Tally 1432.
- Rust tests: `kira-semantics` `tests/specializations.rs` replaces `tests/widening.rs`;
  `backend_parity/widening.rs` deleted and `backend_parity/any.rs` rebuilt its two carriers.
- Docs: `foundation/testing`, `language-guide/structures-and-types`, `language-reference/types`,
  `language-reference/execution-and-feature-status`.

### Backend-parity debt cleared (2026-09-02)
The full `backend_parity` suite passes (450) for the first time since slice 1. The 15 failures it
carried were pre-existing breakage from earlier slices, each fixed at its cause:
- Imported generic templates were unconstructible by their qualified spelling (`Result.Ok(1)` on
  Foundation's `Result`): a row records its template's package-qualified identity and the check
  compared it against the written bare name. `generic_instantiation_expected` now resolves the name
  to the template it names and compares identities. Regression test in `tests/repro_dep_enum.rs`.
- `RUNTIME_ABI_VERSION` 13 -> 14 and the marker renamed to `kira_rt_abi_version_14`. Slice 7 changed
  the `kira_rt_*` symbol set without bumping it, so `.kira-build` native halves were silently reused
  and every hybrid example failed to resolve `kira_rt_native_state_retain`.
- `copy` and `@Derive(Copy)` are two questions again. The derive asserts implicit copying and still
  refuses a `String` member; `copy value` is the explicit clone and accepts every value the language
  deep-copies on bind, refusing single-owner values (native state, cell, task handle, C storage),
  `Any`, and Drop-reaching aggregates. `copy_refusal` walks aggregates itself instead of borrowing
  the derive's stricter walk.
- Stale test expectations corrected to the specified behavior: struct fields drop in reverse
  declaration order, and file text keeps embedded NUL bytes (`text.count` == `size`).
- Test programs and FFI fixtures migrated to required commas and to `charAt` returning `U8`.

### End-to-end suite (2026-09-02)
- The two `tests_verb` failures were this session's: their inline Test family still wrote
  `Result<Int, TestFailure>`. Fixed with the rest of the harnesses.
- The seven `exports`/`ffi_wasm`/`packages`/`web` failures were a missing host toolchain, not code:
  `emcc` was not installed. Emscripten 6.0.9 is now installed at `~/emsdk` and sourced from
  `~/.bashrc`; `kira build --device wasm32` links and node runs the result. All ten web/wasm
  end-to-end tests pass under a shell that sourced `emsdk_env.sh`.
- `kira-clang`'s header test pinned plain `char` to `CXType_Char_S`, which is x86-64's signedness;
  this host is aarch64, where it is `Char_U`. The test names a character type instead.

### kik_harness wrapper (2026-09-02)
- The harness's own `@Main` checksum run trapped on both engines: `trxSection` folded with `* 31`,
  which overflows `Int` once checked arithmetic landed in slice 1. It folds with
  `wrappingAdd`/`wrappingMul`, the idiom every other section already uses.
- The wrapper's pinned tallies were stale (274 ffi, 1420 harness); they are 276 and 1432.
- All 8 `kik_harness` tests pass, including the hybrid FFI harness and the VM/native checksum diff.
- The lifecycle harness asserted an order nothing established: the application thread posted
  `oneOffMainThreadWork` while the main thread was already running the 20000-iteration fiber, so
  which printed first depended on which thread won. Under load, native lost and printed the total
  first. The post now happens from inside the fiber at iteration 10000, which makes the order a
  scheduler property — posted work joins the main thread's one sequence order and runs at the next
  round, before the slices the fiber still owes (`LIFECYCLE_SLICE` is 4096 steps on both engines).
  VM, LLVM, and hybrid all print `main-thread-lifecycle / 42 / manual-main-thread / 20000`.

### Slice 3 completed: runtime type descriptors (2026-09-02)
- `value.type` answers with a `Type`, Kira's runtime type descriptor. Equality is exact
  package-qualified nominal identity. A descriptor exposes `name`, `package`, `kind`, `arguments`,
  and `conformances`, and nothing else (KSEM363); an expression that names no value has no `.type`
  (KSEM362).
- `ErasedTypeId` is now a family word plus a row in a per-program `TypeDescriptorTable`, built
  during lowering while a `distinct` is still itself. One identity now serves erasure, `is`, `as`,
  and `.type`, and a `distinct` keeps its own identity through the representation rewrite.
- The tag an `IntoAny` writes is fixed at lowering rather than derived by each backend from the
  payload's machine type, which is what makes that survive.
- The VM reads a new appended module section; native code runs generated readers over the same
  rows, so no runtime symbol and no ABI version changed. Round-trip, truncation, and
  section-forcing coverage in `module_tests.rs`.
- `type` is accepted as a member name after `.` in the parser and the tree-sitter grammar, with
  corpus coverage. Conformances are recorded once coherence is final and carried through
  `HirProgram::conformances`.
- Coverage: `TdxTypeDescriptorTests.kira` (11 constructs, tally 1443), parity
  `a_runtime_type_descriptor_agrees_on_every_backend`, four semantics tests, docs in the type
  reference, the structures guide, and the feature-status table.

### Native-state owner counting on the VM (2026-09-02)
`a_handle_dropped_with_its_frame_releases_one_owner` had been failing since slice 7 landed, and the
count it caught was real: the VM retained twice for one exported token. A load of a handle already
goes through `Heap::copy_value`, which counts a reference, so `NativeUserData { shared: true }`
adding another left the state one owner over. The VM now takes over the reference the popped value
already carries. `shared` still means what it says on native, where a load is a raw word that took
no reference.

### Test-suite defects found by the unit sweep (2026-09-02)
- Autobind tests fed clang C headers the slice-1 desemicolon pass had stripped, and pinned plain
  `char` to `I8`. C keeps its semicolons, and `char` signedness is the target's: aarch64 binds it
  as `U8`, x86-64 as `I8`, and the test asks the host.
- `kira-launcher`'s alias tests executed a file they had just written while a sibling test was
  between `fork` and `exec`, which is `ETXTBSY`. The run waits that window out instead of reporting
  it as the launcher refusing to run.
- `kira-desktop-runner`'s subprocess test hid the child's exit status when it failed; it prints it
  now.

### Slice 3 completed: a failed cast through `attempt` (2026-09-03)
- `try value as Type` is the cast as a fallible step: it answers `Ok(target)` or
  `Error(TypeCastError.Mismatch(Type))`, and the enclosing `handle` covers it with the rules every
  other failure already has (KSEM139, KSEM141). A cast without `try` still traps.
- `try` now binds looser than `is`/`as`, so `try boxed as Point` is the cast being tried. Parser and
  tree-sitter moved together, with corpus coverage.
- Analysis mints two rows the language owns: `TypeCastError` once per program, and a
  `CastResult<T>` per target recorded as an instantiation of `Kira::CastResult` so its runtime
  identity carries the target. Neither is spellable in source and neither is Foundation's: a cast is
  a language operation, and a failure a program cannot name is a failure it cannot handle.
- One VM instruction (`TypeCastResult`, 0x8f) leaves the payload or the held descriptor under a
  `Bool`, and the bytecode branch wraps whichever it left. Native code branches over the same box
  tag in generated blocks. `Type` became a boxable enum payload on both engines.
- Coverage: three harness constructs (tally 1446), parity
  `a_tried_cast_answers_its_failure_on_every_backend` with heap balance, semantics tests for the
  handled, unhandled, and outside-an-attempt cases, and the statements reference.

## Beyond 1.9.1

Section O's features are in scope for this effort, not deferred: maps and sets, iterators with
declared element ownership, async closures, synchronized shared state, big-endian support, Wasm64,
opt-in runtime reflection metadata, the unsafe-capability model behind packed structs and unions,
versioned hot state migration, and annotation-driven schema evolution. Each ships with the same
proof every 1.9.1 slice ships with: semantics tests, tests-kik coverage, and VM, LLVM, hybrid, and
Web parity where the feature is portable.

### Synchronized shared state: channels
Section O fixes its direction, and the lifecycle harness found the hole it fills: a program cannot
order its own work against a running lifecycle fiber's progress. Until this lands the only fix is to
move the decision inside the ordered context, because an outsider can only enqueue.

Surface: `Channel<T>()` with two owned ends, `Sender<T>` and `Receiver<T>`, each moved into the
context that uses it. `receive` is a suspension point that turns the ordering question into a data
dependency.

Design decisions already made:
- `receive` lowers through the synthesized scheduler IR that `await` uses (`kira-ir/src/tasks.rs`),
  so resumable frames and the parking path are the ones that already work.
- It is a cancellation-observable boundary by construction, which is what section E requires of every
  suspension point.
- Channel rows are generation-tagged and reclaimed like task rows, so a stale end traps rather than
  aliasing reused storage.
- The queue carries `NativeStateValue`s, so VM, native, and hybrid share one representation. New
  `kira_rt_channel_*` symbols are appended and `RUNTIME_ABI_VERSION` goes to 15.
- A closed channel (its sender dropped) yields a typed failure through `attempt`. Never a trap,
  never a sentinel.
- No `tryReceive`. A poll reintroduces exactly the timing dependence the feature removes.

Markers: `Send` guards it and already exists (`Marker::Send`, `refuse_main_thread_value`, KSEM335) —
the ends and the payload must be `Send`, and a non-`Send` payload is refused at the `send` site the
way it is refused at a `MainThread` boundary today. `Sync` is deliberately *not* added: Kira has no
way to share a borrow across threads (no cross-task mutable writeback, task parameters are owned or
copied), so the marker would guard nothing. Add it with shared ownership, which is the change that
gives it a rule to enforce.

Coverage when built: a tests-kik suite proving order between the application thread and a lifecycle
fiber, cancellation while blocked on a receive, the sender dropped while a receiver waits, and
parity on VM, LLVM, and hybrid.
