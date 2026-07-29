# Comparing two `Any` values

`Any == Any` works on the VM, the LLVM backend, and hybrid, and carries a
nominal type identity that neither engine had. Two erased values are equal when
they hold the same Kira type and the same value, so `Point(1, 2)` and
`Rect(1, 2)` are unequal despite matching field for field.

The motivating consumer is a Kira-native test runner: a `Test` case answers with
`Any` and its expectation arrives as `Result<Any, TestFailure>`, so a runner
cannot compare the two without this.

## Why erasure carries a type id

Native code holds an erased aggregate as untyped bytes plus generated
clone/free leaves, tagged only with `ErasedKind` — eight coarse families, one of
which is `STRUCT`. Reading a `Rect`'s bytes through a `Point`'s layout is
undefined behavior, not a wrong answer, so native cannot compare two aggregates
without knowing they are the same type.

The VM fails the same question from the other side. Its heap objects are
structural on purpose — a struct is a tuple of values, no type table reaches the
runtime — so on its own it calls those two structs equal.

`ErasedTypeId` (`kira-semantics-model/src/ty/erased.rs`) answers both. It is a
pure function of `Type`: a family in the high 32 bits, an interned row index in
the low 32. No table and no agreed traversal order, because `Type` is `Copy` and
flat — a struct, array, and enum are each a row index rather than a boxed tree —
so two backends reading the same `Type` compute the same word and cannot drift.

Integer spellings collapse to one id, and float spellings likewise, following
the rule `==` already applies to unerased operands, where numerics unify before
they compare. Structs, arrays, and enums keep their row index.

## What it cost each engine

**The VM.** Erasure used to emit no instruction — a `Value` is a tagged union,
so the erased form of a value *was* that value. That is retired: `IntoAny` emits
`Erase(u64)` and the value lands in an `Object::Erased` box holding the id.
Carrying an erased value still costs only the box; comparing one is what needed
the type. The box counts holds in a plain `shares` field like every other object
on that heap — no `Rc`.

`Widen` had to follow. `Result<Int, E>` -> `Result<Any, E>` was free for the
same reason erasure was, and once erasure boxed, a widened payload stayed bare
where a box belonged — the exact path `expect` takes. The VM now synthesizes a
rebuild helper per `(from, to)` pair (`compile/widen.rs`), mirroring the LLVM
backend's leaf. It has to be a function rather than inline code: `EnumTag` and
`EnumPayload` both consume the enum they read and there is no stack duplication
instruction, so the value needs a local, and a local needs a frame. Ownership
rests on `Interpreter::run` dropping every local of a finished frame, which is
why no path there drops the parameter explicitly.

**Native.** The erasure box's tag is now the `ErasedTypeId` word instead of
`ErasedKind`, which nothing read — same layout, no new field. `kira_rt_any_eq`
compares tags first and only then reads payloads, which is what makes reading
them sound. Aggregates compare through a generated `Leaf::Eq`, the third leaf of
the family `Clone` and `Free` already form; unlike those two it has no
"owns nothing" shortcut, because a flat `memcmp` would read padding a copy never
defines.

## Wire additions, all append-only

No `RUNTIME_ABI_VERSION` bump: opcodes `EQ_ANY` (`0x63`), `NE_ANY` (`0x64`), and
`ERASE` (`0x65`); runtime symbols `kira_rt_any_eq`, `kira_rt_array_eq`, and
`kira_rt_enum_new_aggregate_eq` — appended beside `kira_rt_enum_new_aggregate`
rather than changing its signature, which would have bumped the ABI.

KHM1 gained `internal_functions`. The VM's widen helpers are appended after the
program's functions and belong to no crossing, so the manifest describes a
prefix of the bytecode half — but the count still has to be exact, or the bundle
check could not tell a helper from a stale half carrying an undescribed
function. The trailing sections are positional rather than tagged, so writing
this count while the foreign sections are absent would have the decoder read it
as the foreign-import count; the encoder writes those empty sections explicitly
whenever there is a tail, and omits the whole tail when the count is zero, which
is what keeps a widening-free program's bytes unchanged.

## Verification

2491 tests pass, clippy is clean under `-D warnings`, and `cargo fmt --check`
passes. The new parity cases are in `backend_parity/any.rs`: structural
comparison across every kind, two types with one shape comparing unequal, and a
widened payload equalling a directly erased one — each run on vm, llvm, and
hybrid.

`live_hybrid::a_runtime_only_edit_to_a_hybrid_app_hot_patches` flaked once under
full-suite parallel load (183s) and passes alone in 1.2s. Known timing flake,
not a regression from this work.
