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

### Audit defect closed: the module limit (2026-09-03)
The walk stopped at 1024 modules with a bare `break`, so a program past the limit compiled from the
part that fit: the modules it never read came back as undefined names in files that were correct,
and nothing pointed at the file that was dropped. A test that builds 1024 modules and then one more
proved it — `m1023` simply vanished.

The walk now records that it stopped, `ModuleWalk::overflowed` carries it, and assembly turns it
into `AssemblyError::TooManyModules { reached }`, which the CLI already exits 1 on. `MAX_MODULES`
is documented where imports are.

### Serde step 1: string escaping (2026-09-03)
A `String` value went onto the wire unescaped, so any text carrying `"`, `;`, or `}` parsed back as
structure: the read succeeded and answered something else, which is the one failure a wire format
must not have. Values now escape `\"`, `\\`, `\n`, `\r`, `\t`, and `\0`, the reader undoes exactly
those and traps on any other escape, and the field scanner treats a quoted region as content.
Harness: two constructs in `DnxDistinctTests.kira`, tally 1448 on VM and native.

Note for the next steps: `knvm binstall` copies Foundation into the toolchain, so a Foundation edit
is only visible to `kira test` after a fresh binstall.

### Serde step 2: floats travel as their bits (2026-09-03)
A `Float` is written `0x` and the sixteen hex digits of `floatToBits`, and read back with
`bitsToFloat`. A decimal rendering rounds twice — once writing, once reading — and cannot carry a
signed zero, an infinity, or a NaN payload at all, so `Deserializable` used to refuse floats
outright. It no longer refuses them: struct fields, enum payloads, and a `distinct` over `Float` all
round-trip exactly, proved on VM, native, and hybrid.

Found on the way: the enum reader's fallback constructed the first variant even when it carried a
payload, so any enum whose first variant has one generated invalid code. It now takes a payload-free
variant when the enum has one and otherwise traps before building.

`DeriveSerde.kira` passed the 700-line cap, so the shared reader and writer helpers moved to
`foundation/app/SerdeText.kira` (304 and 461 lines).

Harness: two constructs for escaping and two for floats, tally 1450 on VM and native.

### Serde step 3: integers carry their width (2026-09-03)
An integer is written `U8(7)`, not `7`: the tag names the width and the reader checks it exactly,
then range-checks the digits through the narrowing conversion, so `U8(300)` traps and `Int(5)` in a
`U8` field is refused. `F32` fields, payloads, and distincts are supported with `F32(0x` and eight
digits. Distincts write their name around their representation — `TabId(U32(7))` — per the grammar's
`distinct` production. A `U64` field parses unsigned, because the top half of its range is past what
a signed accumulation and what a literal both reach.
Harness: tagged widths, distinct tagging, payload tagging, and U64-max through text; tally 1454 on
VM and native. Docs and the file header describe the tagged forms.

Repair on the way in: the if/else brace stacks my earlier edits left behind were unmaintainable and
hid a missing close that broke every `Deserializable` derive (which is also why `Result` and the
rest of Foundation went unresolved with it). The three dispatch chains are flat `else if` chains
now. That class of failure is why macro-quote lift failures must become loud diagnostics instead of
silent fallthrough — recorded below with the other two macro-infrastructure fixes.

### Macro infrastructure (2026-09-03)
Three fixes from the Serde work, each proven before it was built on:
- Argument-position splices were never broken. `TabId(#{conv})` renders inline, which a probe
  proved, so the comments claiming otherwise were stale folklore. The distinct reader went from 12
  whole bodies to one body plus a `readInner` fragment, the enum reader arms collapsed onto one
  `conv`, and Hashable's distinct branch did the same. Deleted the false comments.
- `Identifier(text)` is new surface per spec F: it builds the use-site identifier the text spells,
  refused (KMAC013) for non-names and keywords where the text is visible. Proven by Rust tests, a
  `MxxIdentifierNamesAConversion` harness construct (tally 1455), and e2e.
- Unclosed quotes are loud now. `lift` used to stop at the first unmatched brace, leaving every
  later quote raw so the parser reported each surviving `#{` far from the cause. It reports
  (KMAC014) and continues, `compile` surfaces failures with body-relative lines, scan failures
  blank the unscannable tail so no `#{` leaks to the parser, and the unclosed-body messages say the
  rest of the file was skipped. E2e proves an unclosed macro reports KMAC012 with no KLEX001.

### Serde step 4: arrays, nested to any depth (2026-09-03)
`[Int(1),Int(2)]`, `[["a"]]`, `[Point{x:Int(1)}]`: arrays of scalars, strings, nested arrays, and
named derived types round-trip on VM, LLVM, and hybrid, with empty arrays and trailing-comma and
empty-element refusal. The field scanner tracks all three bracket kinds now. Generation is two
recursive comptime helpers (one writes into a caller-owned string, one reads into a caller-named
binding), so one dispatch point serves fields, payloads, and nesting levels alike.
Proving the machinery took a new compiler builtin: comptime `substring`, eval-level only (the
`StringOp` wire format is untouched), with tests and a documented ASCII note.
Harness: three array constructs plus a U64-max case (tally 1458 on VM and native); parity
`serde_arrays_balance_on_every_backend` with heap balance; e2e refusal of `[Option<Int>]` naming
the element rule.

Repair on the way in, and it matters: `Diagnostics.error` records a refusal but evaluation
continues, and the array helpers fail hard (KMAC013) on the same input — discarding the recorded
refusal under their own error. Every array branch now skips codegen once refused, so the refusal is
what surfaces. Any future validation-then-generate macro code must follow the same shape.

### Serde step 5: canonical remainder and the seven-derive surface (2026-09-04)
`@Derive(Serializable)` generates both `serialize_T` and `deserialize_T`.
`Deserializable` is gone rather than deprecated: nothing in the repository
used it once the fold landed, and a reader-only derive that keeps compiling
is the hedge the seven-derive surface exists to remove. Naming it is
`KMAC011`, which points at the derive that exists.
All harnesses, parity programs, and docs derive `Serializable` alone.

The reader consumes the full input: trailing text after the closing `}`
traps, as do a field out of order and a duplicate or unknown label. The
struct envelope check is `pos + 1 == s.count`, so `Point{...}trailing` stops
instead of answering. Parity `malformed_serialized_text_traps_on_every_backend`
covers garbage, truncation, wrong names, non-digits, trailing text, wrong
order, and duplicate labels.

Qualified names are refused with the element rule, not `KMAC013`:
`__kira_serde_ok` rejects dotted spellings, so `[Geo.Point]` reports what the
element holds and generates nothing further. Bare qualified field types were
already refused through the generic path.

The five comptime type helpers moved to `SerdeText.kira` beside the runtime
helpers, so `DeriveSerde.kira` (619) and `SerdeText.kira` (612) both stay under
the 700-line cap. The deserializer body lives once in
`__kira_serde_deserialize_fn`, called by both macros.

Repair on the way in: parity `derives` and `distincts` still expected the
pre-canonical wire (`=`/`;`, untagged integers, bare distincts). They now pin
the canonical text (`:`/`,`, `U8(7)`, `Float(0x...)`, `TabId(U32(7))`). The
derives reference, the tour, the annotations, strings, structures, and types
guides, and the `Derive.kira` header say the same seven.

Deliberately not changed: malformed input still traps rather than returning a
`Result`. Kira has no unwinding, and the round-trip law the spec states is
`deserialize_T(serialize_T(v)) == v`. A `Result` return would rewrite every
call site and the law with it, for data that by construction round-trips.
A typed recoverable failure belongs with schema evolution, which is the
change that gives it a rule to enforce.

Harness tally stays 1458 on VM. The migrated `Dnx` and `Distincts` constructs
prove the fold: every one derives `Serializable` alone and round-trips.

### Channels slice 1: the shared channel table (2026-09-04)
`ChannelExecutor` in `kira-runtime-abi` owns generation-tagged channel
storage with two owned ends per channel. Sends queue FIFO, receives answer
`Value`/`Empty`/`Closed` without blocking, sender drop closes the channel
once the queue drains, and reclamation advances the generation so stale ends
trap as `UnknownHandle`. No wire bytes, no `kira_rt_*` symbols, no Kira
surface yet: the suspension policy and the language surface arrive in later
slices above this table. Nine unit tests pin ordering, closure, wrong-direction
use, reclamation staleness, generation advance, and the zero handle.

The wire contract is pinned alongside the table: `ChannelPrim` answers
`Create`/`Send`/`Poll`/`Take`/`CloseSender`/`CloseReceiver` in that order,
append-only like `TaskPrim`, with round-trip and unknown-byte tests. One
yield cannot carry both a receive status and a value, so a receive is
`Poll` (`0` empty, `1` ready, `2` closed) plus `Take`; generated code never
yields between them, so the pair stays atomic. `perform` is the single entry
point both engines will call. Fourteen unit tests.

### Channels slice 3: the wire layer both engines carry (2026-09-04)
`ChannelOp` is bytecode `0x90`, appended after `TYPE_CAST_RESULT`, carrying
its `ChannelPrim` in a second byte exactly as `TASK_OP` does. `IrExpr::ChannelOp`
mirrors `IrExpr::TaskOp` through lowering, scope, erasure, the callgraph, and
reachability. The VM holds one `ChannelExecutor` per run beside its
`TaskExecutor`, for the same reason: an end handle is an index. Native code
reaches the same table through `kira_rt_channel_op` over a thread-local
executor, reset per run by `kira_rt_channel_reset`, which the generated entry
and the hybrid session scope both call beside the task reset.

`RUNTIME_ABI_VERSION` 14 to 15, marker `kira_rt_abi_version_15`. The guard
proved itself on the way: the LLVM foreign-integration test refused the stale
`libkira_native_bridge.a` by name rather than linking old code under the new
contract. Slice 7's lesson applied without being relearned.

Coverage: bytecode round-trip over every primitive, opcode adjacency, unknown
primitive byte, truncated operand; three native-symbol tests; the shared table
tests above. Full workspace green except `kira-cli`, which is the run below.

### Channels slice 4: the language surface (2026-09-04)
`Channel<T>()` creates a channel and yields its sender; `.receiver` reads the
matching receiver off it. `send(value)`, `receive()`, and `close()` are the
whole surface, and anything else on an end is `KSEM367` — including `.raw`,
which would hand a program the table index and let it forge an end.

The ends are minted `distinct` rows over `Int` filed under `Kira`, following
the `CastResult` precedent rather than adding a `Type` variant. That buys
scalar layout, which is not a convenience but the requirement: an end is moved
into the task that uses it, and a task argument slot is one word. `Sender<T>`
and `Receiver<T>` are spellable as annotations, minted on first mention, so a
function declares an end parameter before the file creating one is analyzed. A
program may still declare its own `Sender`; the rows are owner-filed.

A receive waits. While the queue is empty and the sender is live it hands the
next runnable task a turn and asks again, so it orders the receiving context
after the work that fills it. That policy is synthesized IR — one function per
payload, `__kira_channel_receive_N` — so the VM and the native backend run the
same wait, the argument `kira-ir/src/tasks.rs` makes for the scheduler made
again for the one other place a program blocks.

A drained closed channel answers `ChannelError.Closed` through `attempt`, not a
trap: the sender being gone is an ordinary end to a conversation. Sending to a
channel whose receiver is gone is a trap, because the value has nowhere to
arrive and nobody to tell.

Payloads are one machine word: an integer width, a float width, `Bool`, or a
`distinct` over one (`KSEM365`). A `Float` crosses as its exact bits and is
converted at both ends, exactly as a task argument is; a `distinct` keeps its
identity because the row carries the declared type while the wire carries the
representation.

Repair on the way in: `task_scalar` was a free function with no type table, so
it refused a `distinct` over `Int` in a task slot. A distinct is erased before
IR exists, so one crossing a slot is already the word its representation is;
refusing it meant a channel end could not reach the task that uses it, which is
the only place an end is ever going.

Coverage: `ChxChannelTests.kira`, seven constructs (tally 1465 on VM and
native) including the ordering case, the closed case, float and distinct
payloads, and two channels not sharing storage; parity
`a_channel_orders_two_contexts_on_every_backend` with heap balance; ten
semantics tests for the refusals; `KSEM364`-`KSEM367` in the registry and the
diagnostics appendix; the concurrency guide and the feature-status table.

### The desktop runner cut its app off (2026-09-04)
`a_server_that_vanishes_after_the_app_is_up_leaves_the_runner_clean` was
recorded as a flake. It reproduced 50/50 once the tree was quiet, and it was
a real defect, not timing noise.

`RunnerHost::start` returns when the entrypoint is *running*, deliberately:
`RelayHost` acks the protocol thread before handing the app thread the run,
because that ack is the last moment the session can be told the app never
started. So `entrypoint started` means started. When the server then vanished,
the runner reached `std::process::exit` while the app thread was still inside
the entrypoint, and the app's output was whatever had reached the pipe. Half
the runs it was nothing.

`RunnerHost::settle` waits for the app to come back to rest, defaulting to a
no-op for a host whose entrypoint runs on the calling thread. `RelayHost`
implements it as a `Work::Settle` request carrying no work: the app thread
takes one request at a time, so an answer to it cannot arrive before the
request ahead of it returned. No polling, no second copy of the app's state.
The runner settles before exiting. Eight consecutive clean runs, and a unit
test pins the queue-position claim by making the request ahead of it slow.

### Workspace lints cleared (2026-09-04)
`cargo clippy --workspace` is clean. Nineteen warnings were standing:
`chunks_exact` with constant sizes across the two SHA-256 implementations, the
SPIR-V word packer, and three wire decoders (`as_chunks`, which also drops a
copy per block); five unnecessary qualifications in the macro evaluator, from
this effort; four redundant match guards on `IntSpelling`/`FloatSpelling`
that are patterns; a manual range check in the VM's float-to-int narrowing;
and `Staged::VmLoaded` carrying a 248-byte `Module` inline in an enum the
runner holds for its whole life, now boxed.

## Beyond 1.9.1

Section O's features are in scope for this effort, not deferred: maps and sets, iterators with
declared element ownership, async closures, synchronized shared state, big-endian support, Wasm64,
opt-in runtime reflection metadata, the unsafe-capability model behind packed structs and unions,
versioned hot state migration, and annotation-driven schema evolution. Each ships with the same
proof every 1.9.1 slice ships with: semantics tests, tests-kik coverage, and VM, LLVM, hybrid, and
Web parity where the feature is portable.

### Synchronized shared state: channels (landed 2026-09-04)
Section O fixed the direction, and the lifecycle harness found the hole it fills: a program cannot
order its own work against a running lifecycle fiber's progress. Until this landed the only fix was
to move the decision inside the ordered context, because an outsider can only enqueue.

Built as planned: `Channel<T>()` with two ends each moved into the context that uses it; `receive`
as a suspension point that turns the ordering question into a data dependency, lowering through the
synthesized scheduler IR `await` uses; generation-tagged rows reclaimed like task rows, so a stale
end traps rather than aliasing reused storage; a closed channel yielding a typed failure through
`attempt`, never a trap and never a sentinel; no `tryReceive`, because a poll reintroduces exactly
the timing dependence the feature removes; `kira_rt_channel_*` appended with `RUNTIME_ABI_VERSION`
at 15.

Two deviations from the plan, both deliberate:
- The queue carries one machine word rather than a `NativeStateValue`. Scalar layout is what lets an
  end and its payload cross a task argument slot, and a queued pointer into the sender's heap would
  hand the receiver storage the sender still owns. Heap payloads are the next slice and need the
  value-tree representation the seam already has; the refusal is `KSEM365` and names the rule.
- The ends are minted `distinct` rows rather than a new `Type` variant, following `CastResult`.
  Nominal identity and scalar layout both come out of that with no new variant to teach 21 files.

`Send` is not yet enforced on the payload. Every type the payload rule admits today is `Send`, so
nothing unsendable can cross; the check becomes load-bearing with heap payloads and lands with them,
the way the task-slot rule and `KSEM312` already sit beside each other.

Still to prove: cancellation while blocked on a receive, and ordering against a lifecycle fiber
specifically rather than against a task.


### Channels slice 5: the deadlock fix run, and what running it found (2026-09-05)
The trap written in slice 4 had never been executed. It works: the hang repro traps in well under
a second on both engines, and `a_receive_nothing_can_answer_traps_on_every_backend` covers it.
Three things came out of actually running it.

**The two engines said different sentences about the same trap.** The VM printed
`kira: runtime trap: channel trap: a receive is waiting for a value nothing can send` and native
printed the same trap without the category word. Every other native trap prints unprefixed, so the
VM was the outlier; `VmError::Task` and `VmError::Channel` now render the trap and nothing else.
`assert_trap_message_parity` pins the sentence itself across vm, llvm, and hybrid, because agreeing
on which programs trap is worth nothing if each engine dresses it in its own words.

**A lifecycle fiber could not use a channel or a task on the VM.** `Fiber::step` builds a fresh `Vm`
per slice, and the task and channel tables lived on the `Vm`, so both were rebuilt at every
suspension while the fiber's heap and locals survived. Native keeps them, being thread-local, so
this was also a backend divergence. `VmExecutors` is now the fiber's, handed to the VM at the start
of a slice and taken back on every path out including a trap, the way the heap already was. The
lifecycle harness spawns a task and receives from a channel across 20000 iterations of the loop,
which is many slices, so the rows have to be the same rows on both sides of every boundary.

**`Send` is enforced on the payload.** It runs before the representation rule, exactly as the
task-slot pair runs it: a function type is refused for what it kept (`KSEM312`), not for its width.
`Void` and pointer words join `KSEM365` — a channel over `Void` carries no value, and a pointer word
names storage this language does not read. An end written as an annotation refuses the same payloads
under the same code instead of falling through to "`Sender` is not generic", and still falls through
when the program declares a template of that name.

Cancellation while blocked is covered: `ChxACancelledFillerLeavesTheReceiveUnanswerable` calls off
the only work that could fill the channel, so the wait has no turn to hand out and the receive learns
its value is not coming; `ChxACancelledFillerDoesNotStopALiveOne` proves cancelling one filler is not
cancelling the channel.

Still open: heap payloads. The shape is settled — the queue holds a `NativeStateValue` rather than a
word, filled by the VM's `Heap::into_native_state` and native's `encode_native_state_value` and
drained by their inverses, which means two table methods outside the `(Int, Int, Int) -> Int`
primitive contract, two bytecode instructions, two `kira_rt_channel_*` symbols, and an ABI bump. The
work in it is not the transport but the ownership: `send` has to consume, a closed receiver has to
drop what it discards, and the heap-balance assertions in the parity suite are what will say whether
it does.

### The §1c gate, watched (2026-09-06)
The deadlock fix had been written, committed, and never run. It runs. The
handoff's repro — a receive on an empty live channel with nothing runnable —
traps on all three engines with one sentence:

```
kira: runtime trap: a receive is waiting for a value nothing can send
```

`vm` under `timeout 10`: exit 1, under a second. `llvm`: exit 1, 2s. `hybrid`:
exit 1, 1s. The previous behavior was exit 124 at the bound, so the bound is now
slack rather than the thing being measured.

Hybrid said it differently until this slice. Every hybrid failure went through
one `err!("kira: {error}")` — a manifest that would not decode and the program
trapping worded as one kind of thing — so a trap arrived as `kira: a receive is
waiting…` with the two words that name it as a trap missing.
`kira_hybrid_runtime` already separates the last case as `HybridError::Trap`;
`HybridError::report` reads that distinction rather than flattening it.

Harness on `--backend vm`: **1468 passed, 0 failed, 0 skipped, 1468 total**,
which is the pin. All ten `Chx` cases pass, including the three added this
slice: the bare deadlock, the cancelled filler that leaves the receive
unanswerable, and the cancelled filler that does not stop a live sibling.

Harness on `--backend llvm`: **1468 passed, 0 failed, 0 skipped, 1468 total**,
the same tally over an identical set of case names. The two engines collect and
pass the same 1468 cases.

Lifecycle harness on `vm`, `llvm`, and `hybrid`: identical output
`main-thread-lifecycle / 42 / manual-main-thread / 20008`, exit 0 on each. The
`20008` is the loop's own 20000 plus the 7 the channel carried and the 1 the
task returned, so a lifecycle that lost either row at a slice boundary could
not print it.

`cargo test -p kira-cli --test backend_parity`: **455 passed, 0 failed**, 882s.
That is the handoff's 454 plus `a_receive_nothing_can_answer_traps_on_every_backend`,
which now asserts the trap's sentence and not only its exit status, on vm, llvm,
and hybrid alike.

The suite had never compiled on this branch: `assert_trap_message_parity` was
added to the harness and called from `tasks.rs` without being imported there.
Running it is what found that.

`cargo test -p kira-cli --test kik_harness`: **8 passed, 0 failed**, 1971s. That
is the 1468 pin asserted whole on both engines, the checksum run agreeing byte
for byte, the lifecycle output, the ffi harness at 276, and the syscall
harnesses. Unit suites for the crates this slice touched:
`kira-semantics` 845, `kira-vm-runtime` 116, `kira-runtime-abi` 173, all with
zero failures.

The wasm end-to-end tests were not run: neither Emscripten nor node is present
on this host, so they are unrunnable here rather than passing or failing.
