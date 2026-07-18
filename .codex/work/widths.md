# Fixed-width scalars

`I8`..`I64`, `U8`..`U64`, `F32`, `F64` landed on VM, LLVM/native, hybrid, and
wasm.

## The oracle answer that decided the whole design

The task framing assumed per-width storage and per-width wrapping. The oracle
has neither, and finding that out first turned a full horizontal slice into a
mostly-frontend change.

`packages/kira_semantics/src/lower_shared_type_text.zig:171-182` resolves every
fixed-width name to `.kind = .integer` (or `.float`) carrying a `name` string.
There is no separate kind, so there is no separate representation:
`packages/kira_vm_runtime/src/vm_interpreter.zig:227-266` adds with `+%`,
subtracts with `-%`, multiplies with `*%` on `i64`, and nothing anywhere masks a
result to 8, 16, or 32 bits. **A `U8` sum of 250 and 10 is 260, not 4.**

The width decides exactly two things.

**Distinctness**, via `ResolvedType.eql`
(`packages/kira_semantics_model/src/types.zig:42-57`). `require_exact_name` is
false for `.integer`, so the rule falls out of the null-name branches: two
*named* widths compare by name (`U8` ≠ `I64`), while a null name matches
anything. An integer literal is `.{ .kind = .integer }` with no name
(`hir.zig:589`) — which is the entire mechanism behind `let x: U8 = 5`. No
implicit conversion, no polymorphic-literal machinery: the literal is simply
assignable to every width.

**Signedness of `/`, `%`, and ordering.** `isUnsignedIntType`
(`packages/kira_ir/src/lower_from_hir.zig:1016-1021`) is "the spelled name
begins with `U`". The oracle's own comment at `ir.zig:405-407` says add,
subtract, and multiply ignore it because they "wrap identically for both
signedness" — so only six operators are affected, and equality is not one of
them: the same 64 bits compare equal either way.

## What that made the design here

`Type::Int(IntSpelling)` / `Type::Float(FloatSpelling)` — a payload on the
existing variants rather than new variants. Deliberate: it makes every one of
the 116 existing `Type::Int`/`Type::Float` sites a compile error, so the
compiler visits each one instead of new variants silently falling through
`is_numeric`, `is_printable`, and the backend type maps. Two wasm sites and one
LLVM site were caught this way after a mechanical pass wrote `Type::INT` (the
plain-spelling const) into a *pattern* position, where it compiles but means
"plain `Int` only" — the non-exhaustive-match error is what flagged them.

`assignable_to` reproduces the oracle's wildcard rule and is therefore
**non-transitive**: `U8` → `Int` and `Int` → `I64` hold, `U8` → `I64` does not.
Reproduced, not smoothed over.

`unify_numeric` in `kira-semantics/src/operators.rs` is new and load-bearing.
The operator tables previously demanded `lt == rt`, which the wildcard breaks:
`u8Value + 1` has a `U8` and a plain `Int` operand. Unification yields to the
written width, so the result of `u8Value + 1` is `U8` and stays unsigned.

## Not desugared

No desugar was available. A width is a property of the type lattice, and
signedness picks a different machine instruction — neither is expressible as a
rewrite over existing HIR. This is one of the features that genuinely needs the
slice.

## The one wire-format change

Six opcodes appended after `ENUM_PAYLOAD` (0x37): `DIV_UINT` 0x38, `REM_UINT`
0x39, `LT_UINT` 0x3a, `LE_UINT` 0x3b, `GT_UINT` 0x3c, `GE_UINT` 0x3d. Appending
is not an ABI change, so `RUNTIME_ABI_VERSION` is untouched and no `kira_rt_*`
signature moved. No unsigned add/sub/mul and no unsigned equality: each would
duplicate an existing opcode.

Unsigned division is a *shorter* lowering than signed in every backend, not a
longer one — no unsigned pair overflows, so the `MIN / -1` branch the signed
path needs has no twin. Only the divide-by-zero trap remains, and it is the same
trap on all four backends.

## Refused, with the reason

- **Per-width wrapping.** The oracle wraps at 64 bits for every spelling.
  Masking a `U8` to 8 bits would be inventing language surface, and it is pinned
  against by `arithmetic_wraps_at_64_bits_for_every_spelling`.
- **`Byte` as a builtin.** `isBuiltinTypeName`
  (`lower_shared.zig:697-707`) does not list it; the corpus declares it as
  `type Byte = U8` (`tests-kik/harness/app/aliases/Aliases.kira:4`). Type
  aliases already landed here, so `Byte` works today without a ninth hardcoded
  integer name. `there_is_no_byte_builtin` pins that it is not one.
- **`I128`/`U128`/`Char`.** Absent from the census; they do not resolve.
- **`CBool`/`CString`/`RawPtr`.** Builtins in the oracle, but C-seam types
  rather than numeric widths — they belong with the FFI work, not here.

## Files split on the ladder

Both were pushed past 700 lines by this change:

- `kira-llvm-backend/src/codegen/lower/expr.rs` 735 → 367, with operator
  lowering moved to `lower/operators.rs` (386).
- `kira-wasm-runtime/src/lower.rs` 706 → 565, with binary-operator lowering
  moved to `lower/operators.rs` (166).
