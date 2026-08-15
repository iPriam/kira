# Post-desugar checking and the retirement of backend refusals

## Decision

Do not add a `kira-mir` crate. `kira-ir` already is the mid-level IR: all three
backends consume `IrProgram`, and `kira-ir/src/mid.rs` already establishes the
pattern of answering a question once so both engines read one answer. A second
mid-level crate beside it would manufacture the drift this work removes.

The work is to move invariants onto the lowered form and verify them once.

## Evidence

`crates/kira-ir/src/mid.rs` states the case in the repository's own voice, about
release planning:

> Two hand-maintained answers to one question, with nothing checking they agree,
> which is the shape a leak drifts into. One side gains a case the other does
> not, and the program leaks on that backend only, in programs no test happens
> to run.

Every backend refusal listed below is that same shape, not yet generalized.

## Prerequisite: spans in the IR

`grep -c Span crates/kira-ir/src/ir.rs` returns **0**. `kira-source` appears in
that crate's tests only, and `kira-ir` does not depend on `kira-diagnostics`.

This is why every rule lives in `kira-semantics`: it is the only layer that can
point at a line. Nothing downstream can diagnose, so each backend re-derives
what it needs and fails without a caret.

The same absence causes three other defects:

| Defect | Cause |
| --- | --- |
| `DebugInfo::declaration_line` scans source text for a function's name | no span on `IrFunction` |
| `kira profile annotate` prints one declaration line against every instruction | DWARF/CodeView get one line per function |
| bytecode carries no line table | same |

Fixing spans pays for itself in debug information before any verifier exists.

## Sequence

1. **Spans into the IR.** `IrFunction`, statements and expressions carry a
   `FileSpan`. `kira-ir` gains a `kira-diagnostics` dependency; that is layer 3
   depending on layer 0, so the direction is legal. Lowering already knows each
   span at the point it builds the node.
2. **`kira-ir::verify`.** One pass proving what the backends assert privately.
   Run it in `kira-build` after lowering, reporting through `kira-diagnostics`
   with carets.
3. **Retire the backend refusals.** Each becomes a verifier rule or is deleted
   as impossible by construction. What remains in a backend is only an internal
   error, which should then be unreachable rather than merely rare.
4. **Move post-desugar rules off `kira-semantics`.** Any rule whose subject is
   created by desugaring belongs on the verifier.

## Inventory

| Backend | Count | Where |
| --- | --- | --- |
| LLVM | 94 `Unsupported(` occurrences | `crates/kira-llvm-backend/src`, densest in `codegen/lower/foreign_aggregate.rs` (25) and `codegen/lower/call.rs` (10) |
| VM | `StructAtSeam`, `ArrayAtSeam`, `EnumAtSeam`, `NativeStateOperation`, print refusal | `crates/kira-vm-runtime/src/error.rs` |
| hybrid | `UnsupportedOwnership` | `crates/kira-hybrid-runtime/src/error.rs` |

Three categories, three treatments:

- **Internal invariants**, the large majority. Names like "a call to an unknown
  function", "a struct the module never declared", "a value with no type".
  These are not features a user can avoid. They become verifier rules, and the
  backend keeps `LlvmError::Internal` for the case where the verifier passed and
  the invariant still did not hold.
- **Genuine limits**, roughly eight. Aggregate larger than 4 GiB, offset past
  4 GiB. These deserve typed errors that name the limit.
- **Reachable from source.** Investigate each against the other two backends
  before deleting: if one backend supports it and another does not, that is a
  cardinal-rule break and the answer is to implement, not to refuse.

### Known cardinal-rule suspect

`LlvmError`'s comment says struct, array and enum crossings at the
`@Native`/`@Runtime` seam now travel as a node tree, and that the VM's
`StructAtSeam`/`ArrayAtSeam`/`EnumAtSeam` are a different question, about a
value with no tree form rather than a type with no crossing. But
`VmError::ArrayAtSeam`'s own message reads "passes an array across the native
seam, which cannot carry one yet", and the doc above it calls itself "a gap
rather than" a value-shaped refusal. One of those two statements is stale.
Settle it before writing verifier rules for the seam.

## What stays in kira-semantics

Name resolution, type checking, ownership and borrow rules: anything whose
subject exists before desugaring. Only rules whose subject is *created* by
desugaring move, which today means capture cells from closure lifting, closure
representation and dispatcher structs, release plans, and widening rows.

## First rule to move: gone, not moved

`Analyzer::recheck_native_state_sites` is deleted. It existed only to catch the
capture cell that closure lifting introduced into a boxed type, and callback
state now *carries* a cell — see `callback-state-capture-cells.md`. The rule it
replayed no longer refuses anything, so there was nothing left to move.

What replaced it is the shape the verifier wants anyway:
`Analyzer::finalize_native_state_type_ids` runs after `finalize_closures` and
answers a question over the finished program rather than replaying a remembered
list of spans.

## State of the tree at handoff

Landed and verified this session, none of it committed:

| Change | File | Verified by |
| --- | --- | --- |
| `default_value` picks the first terminating enum variant | `kira-semantics/src/closures/calls.rs` | `kira check` on kira-ui went from stack overflow to `ok:` |
| codegen workers get a 64 MiB stack | `kira-llvm-backend/src/lib.rs` | navigation-app got past `generating native code` |
| cells compare by identity, matching the VM | `kira-llvm-backend/src/codegen/values.rs` | navigation-app builds on LLVM |
| `nativeState` re-checked after lifting | `kira-semantics/src/typeck/native_state.rs` | one `KSEM214`, identical on all three backends |
| `LlvmError::Internal` names the invariant it found | `kira-llvm-backend/src/lib.rs` | named `Cell(CellId(7))`, which is what identified the bug |
| Windows stack reserve for toolchain binaries | `.cargo/config.toml`, `kira-toolchain/src/lib.rs` | read back from the PE optional header |
| `KSEM272` on an enum with no finite value | `kira-semantics/src/enums.rs` | caret at the declaration, from the installed `kira check` |

The workspace gate is green: 3287 tests, formatting, clippy, the WASM core
check, and the file-size rules. The `Tree` program runs on VM, LLVM and hybrid
and prints `leaf` on each.

### Two gate repairs the parity check exposed

`kira_dev_validate {suite: "backend_parity"}` ran `cargo test -p kira-cli --
backend_parity` and matched **zero** tests: a test binary's name is no part of
any test's name, so the filter selected nothing and the suite reported success.
`golden` had the same shape. `suite.rs` now distinguishes `Narrow::Name` from
`Narrow::Target` and spells `--test` for the latter, which runs the 398 parity
tests and the 4 golden ones. `Outcome::success` no longer reads the exit status
alone: a named suite that matched no test is a `CapabilityMissing` failure,
because a selection that proves nothing must not pass a gate.

`knvm_sinstall`'s `sinstall_lands_both_tools_and_configures_the_path` installed
for real without taking `UserPathGuard`. On Windows that edits the user's
persistent `Path` and never restored it, so it raced the guarded test beside it
and leaked a dead temp entry into the developer's environment. The guard is now
file-scope and every installing test holds it.

Also uncommitted: the `kira-profile` crate and the `kira profile` verb, the
`kira-instruments` deletion, and the `pipeline.rs` split.

### Not this session's work

`crates/kira-debug/src/engine.rs`, `crates/kira-debug/src/dap/` and the
`lldb.rs` rewrite are another session's. `kira-debug` compiles now and the
workspace gate covers it. Do not revert it without checking who owns it.

## The `default_value` fix is pinned

`closures::a_dispatcher_with_no_implementations_returns_a_finite_value` reaches
it through the fourth caller, not the construct ones: a function type that is
*called* with no literal anywhere in the program mints a dispatcher whose body
nothing can reach, and that body still needs a well-typed `return`. A `Tree`
result whose first variant carries a struct holding a `Tree` is the shape, and
the test asserts the chosen tag is `Leaf`. With the visited set removed it
overflows the stack, so it fails against the bug rather than merely passing
beside it.

`KSEM272` closes the hole the fix left open. `default_value`'s comment named
`Analyzer::check_enum_terminates` as the pass that reports an enum with no
finite value, and no such pass existed: an enum every one of whose variants
leads back into itself was accepted silently and reached the desugar as an
`Error` node. It is now reported at the declaration by `enums.rs`, which runs
after every body — so a generic instantiation is covered — and breaks the
reported enum's payloads to `Error` the way `KSEM052` breaks a struct field.
An enum with *no variants* is exempt: a construct family that no declaration
backs is exactly that shape, and it is uninhabited by declaration rather than by
mistake.

One walk answers the question for both: `Analyzer::has_finite_value` asks
`default_value_inside` and discards what it built, rather than growing a second
walk over the same shape beside it.

## Verification

Use the installed toolchain, not `target/debug/kira.exe`:

```sh
knvm binstall
cd ../kira-ui && kira check                       # was a stack overflow
cd Examples/navigation-app && kira build --backend vm
```

All three backends must give the same answer for the same program. That is the
property this work exists to make true by construction rather than by testing.
