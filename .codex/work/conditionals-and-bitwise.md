# Conditionals and bitwise operators

**They execute. They are not refused.** The brief for this work assumed both
were frontend-only surface in the oracle — the conditional documented with no
corpus call site, the bitwise operators holding tokens with none. That is true
of the *corpus* and false of the *implementation*, and the implementation is
what settles it.

`docs/language_inventory.md` draws the line explicitly, and both fall on the
executable side of it: "conditional expressions in the lowered scalar/pointer
subset" is listed in the Current Executable Lowering Boundary. Tracing the
operators confirms the same for the bitwise set — `lower_from_hir.zig:927`
lowers `bit_and`/`bit_or`/`bit_xor`/`shift_left`/`shift_right`,
`vm_values.zig:118` evaluates them, and `backend_capi_codegen.zig:317` emits
`LLVMBuildAnd`/`Shl`/`LShr`/`AShr`. So the precedent that governs `print(struct)`
and structs at the native seam does not apply: nothing here is unpinned, and
refusing them would have invented a restriction the oracle does not have.

## Not desugared, and why

The conditional is the one construct on the recent list that a desugar in
`kira-semantics/src/stmt.rs` could not absorb. `for` → `while` and `switch` →
if/else both work because both are *statements*, and a statement rewrite can
introduce statements. A `? :` is an expression that may sit anywhere — inside a
call argument, inside arithmetic, inside another conditional — so rewriting it
into an `if` over a temporary would need statement hoisting out of arbitrary
expression position, which this lowering deliberately does not do.

What made it cheap anyway is that every backend already branches at expression
level for `&&`/`||`. `HirExpr::Select`/`IrExpr::Select` reuse that machinery
rather than adding any:

- **bytecode**: `compile_select` is `compile_and` with both arms carrying a
  value — condition, `JumpIfFalse`, then-arm, `Jump`, else-arm, patch. **No new
  opcode.**
- **LLVM**: a branch and a phi, deliberately *not* `LLVMBuildSelect`, which
  evaluates both operands.
- **wasm**: a value-typed `if`/`else`/`end`, which is exactly the shape.

The bitwise operators do need opcodes, and got seven appended after `GE_UINT`
(`0x3e`–`0x44`). Append-only, so no ABI bump and no marker rename.

## The three rules that had to agree across four backends

**Shift count modulo 64.** The VM's `wrapping_shl`/`wrapping_shr` mask by 63 and
wasm's `i64.shl`/`shr_s`/`shr_u` do the same, but LLVM's `shl`/`lshr`/`ashr` are
**poison** for a count of 64 or more. The native backend therefore masks the
amount with an explicit `and 63` before shifting. Without it a program using an
out-of-range count would silently produce a different answer natively than on
the VM — the kind of divergence a parity test exists to catch, which is why
`an_oversized_shift_count_wraps_modulo_sixty_four` is there.

**`>>` is the only shift with two forms.** `&`, `|`, `^`, and `<<` are
bit-identical under either signedness and need no unsigned twin, exactly as
`+`/`-`/`*`/`==` did not in the widths work. What fills the vacated high bits of
a right shift is precisely the question signedness answers, so `ShrInt` and
`ShrUInt` are separate, selected from the **left** operand's spelling.

**Bitwise binds looser than equality.** This is the oracle's ladder
(`parser_types_exprs.zig`: conditional → `||` → `&&` → `|` → `^` → `&` →
equality → comparison → shift → term → factor), not C's, and the difference is
observable: `flags & 8 == 8` groups as `flags & (8 == 8)` and is a **type
error** rather than a different number. Splitting equality and comparison into
separate rungs while renumbering also fixed a pre-existing divergence — both sat
at binding power 3 here, where the oracle has always had equality looser.

## What the tests caught

The wasm execution cases earned their place immediately: a conditional yielding
a `String` validated on wasm32 and failed on wasm64, because `val_type` names
the *narrow* type and a heap handle is as wide as the memory is. The fix is to
ask `value_of` instead. Nothing in the type system or the VM would have surfaced
that.

The parity suite caught the second one, in the test rather than the compiler:
`bits & 8 == 8` inside the seam case failed to compile on all three backends
identically — parity held, the expected output was wrong. The precedence rule
working as designed.

## Left alone

Nothing was refused. Two smaller notes:

- `>>` lexes as one token, matching the oracle. When generics land,
  `Foo<Bar<Int>>` will need the parser to split it, as the oracle's will.
- `Type::Void` branches are rejected (`KSEM133`) rather than lowered. Two `Void`
  arms type-agree but leave nothing for the surrounding expression to consume,
  and no backend can represent that; the oracle has no corpus site either way.
