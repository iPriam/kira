# Arrays: landed on all four backends

**Status: landed.** The frontend, the VM, both wasm address widths, and the
LLVM/native backend are complete and green, and parity is proven differentially
— `backend_parity/arrays.rs` runs the 18 shapes on VM, LLVM, and hybrid, the
wasm suite runs them on wasm32 and wasm64, and `examples/arrays/arrays.kira` is
parity-checked for free. This note is the design behind that work.

## The decisions worth keeping

### `Type` stays `Copy`: arrays are interned, not boxed

`Type::Array(ArrayId)` indexes an `ArrayTable` that interns on the element type,
so `[Int] == [Int]` is a `u32` compare and `[[Int]]` is a row whose element is
the row for `[Int]`. A `Box<Type>` would have cost `Copy` on every type in the
lattice to express a handful of array types.

`StructTable` and the new `ArrayTable` are owned by one `TypeTable`, because
`type_name` and `owns_heap` stop being answerable by either alone once
`[SomeStruct]` exists. `HirProgram.structs`/`IrProgram.structs` became
`.types`.

### `TypeRef` became an arena node

`[[Int]]`'s element is itself a written type, so the flat `Copy` struct could not
express it. It follows the index/arena law — `TypeRefId` into
`SyntaxTree::types`, never a `Box`. The `Error` variant is new and load-bearing:
a malformed type resolves to `Type::Error` **silently**, where the old sentinel
`Symbol::ERROR` produced a cascading "unknown type `<error>`".

### `for x in xs` is a desugar — zero backend work

In `kira-semantics/src/stmt.rs`, exactly as `for`-over-range and `switch` are:

```text
let  <array> = xs              // hidden: evaluated once
let  <limit> = <array>.count   // hidden: measured once, not per test
var  <cursor> = 0
while <cursor> < <limit> {
    let x = <array>[<cursor>]  // immutable, a copy
    <cursor> = <cursor> + 1    // before the body: `continue` must not skip it
    body
}
```

No IR node, no opcode, no backend arm. The hidden `let <array> = xs` does **not**
consume `xs`: `apply_binding_move` runs only on a `let` the user wrote, and this
statement is built by the desugar. So `for x in xs { }` leaves `xs` usable, which
is what a reader expects of a loop that only reads.

### `.append` resolves a place; `.count` does not

Reading an array yields an independent copy, so appending to a *read* would push
onto something discarded a moment later and lose the write with no diagnostic.
`HirExpr::ArrayAppend` therefore carries a `HirPlace`, not an expression. That is
the whole reason `rows[0].xs.append(42)` lands in `rows`. `.count` only reads, so
it takes any expression.

`HirPlace`/`IrPlace` grew `Field | Index` steps. An `Index` step holds an
*expression* — its value only exists at run time — which is why it is an enum and
not a list of numbers.

### Evaluation order is fixed in the IR, not per backend

`IrPlace` states it: a place's index expressions are evaluated left to right, and
all of them before the assigned value. Every backend follows it, which is what
keeps `xs[next()] = next()` agreeing.

### A negative index is a different trap from out of bounds

The oracle draws this line (`vm_interpreter.zig` traps "array load requires a
valid array handle and index" for negative, "array index is out of bounds" for
past-the-end), so this does too. The VM's message deliberately does **not** name
the offending index: a wasm trap path cannot format one without allocating
mid-trap, and the oracle does not name it either — so naming it would have been
invented detail bought at the price of parity. That was caught by a failing
differential test, not by inspection.

### Refusals, with their reasons

- **`print(someArray)` → `KSEM081`.** Same evidence as `print(struct)`: no corpus
  call site, no golden file naming a separator or a bracket. Every one of those
  is a decision the language has not made.
- **`copy xs` → `KSEM116`.** There is no array clone. Not invented here.
- **An array at the `@Native` seam → refused.** This one is a **gap, not a
  decision**, and differs from the struct refusal: the language *does* let an
  array cross. What is missing is the ownership answer at the boundary — who
  frees the elements, and what a native callee growing the array means for the
  VM's heap accounting. A wrong answer there is a double free or a leak at the
  boundary. `BridgeValueTag::ARRAY` (6) is appended so a manifest can *describe*
  one; every marshalling path rejects it.

### The oracle question that decided the runtime

`copy_indirect` deep-copies an array field rather than sharing the handle. Both
oracle backends agree and it is what makes affine copy semantics work. So
`Heap::copy_value`'s array arm deep-clones, the struct arm reaches it by
recursion, and `current == 0` at exit still proves balance. The corpus has no
test for it, so `copying_a_struct_deep_copies_an_array_field` in
`kira-vm-runtime/src/value.rs` pins it directly, and
`copying_a_struct_does_not_alias_its_array_field` pins it differentially on wasm.

## The cost this buys, and why it is not fixed here

Reading a local copies it, so `xs[i]` inside a loop deep-copies the whole array:
**an array loop is quadratic**. That is the existing cost model — a struct field
read already deep-copies its struct — and the fix is the by-reference load the
`borrow mut` work needs (see `ownership.md`), not a special case in the array
arm. Recorded rather than papered over.

## What is done

| Layer | State |
|---|---|
| `kira-syntax-model` | `TypeRef` arena node, `Expr::ArrayLit`/`Index`, `ForIterable::{Range,Each}` |
| `kira-lexer` | nothing — `[`/`]` already lexed |
| `kira-parser` | recursive types, literals (commas optional), index postfix, `for` iterable; 62 tests |
| `kira-semantics-model` | `ty/` split into `mod`/`structs`/`arrays`/`table`; 4 HIR nodes; place steps |
| `kira-semantics` | new `arrays.rs`, `place.rs`, `types.rs`, `stmt/fors.rs`; `for`-in desugar; `tests/` split by topic |
| `kira-ir` | 4 IR nodes, `IrPlaceStep`, `place_type` walk |
| `kira-bytecode` | opcodes `0x30..0x34` appended, encode/decode/validate |
| `kira-vm-runtime` | `Value::Array`, `Object::Array`, deep copy/drop, dispatch; 26 tests |
| `kira-wasm-runtime` | `rt/array.rs`, `arrays.rs`, `lower/places.rs`, all 3 depth walkers; execution tests green on wasm32 + wasm64 |
| `kira-native-bridge` | `array.rs`: the full native runtime, aborting on OOM/overflow as `Vec` does; 8 tests green |
| `kira-llvm-backend` | `elements.rs` (element leaves), `values.rs` (deep copy/drop on `Codegen`), the four expr arms, and the `Field \| Index` store walk |

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` are green on the no-LLVM path, and the LLVM feature path
(`--features kira-cli/llvm`, clippy and the full test suite including
`backend_parity/arrays.rs`) is green with a real LLVM present.

## How the LLVM backend emits an array

The loop stays in Rust and LLVM emits only *leaves*, the same split
`kira-native-bridge`'s `array` module was designed around:

- **`values.rs`** carries `copy_value`/`drop_value` on `Codegen` (not on one
  function's lowering), because the same deep walk that copies a local read also
  fills the clone/free leaf `elements.rs` emits — and a leaf is built with no
  function body in scope. `Codegen` gained a `target_data` (borrowed from the
  module, for `LLVMABISizeOfType`) and an `element_leaves` cache keyed by
  `(element type, Leaf)` so two arrays of one element share a leaf.
- **`elements.rs`** emits the per-element clone/free leaf, or a **null** pointer
  when the element owns nothing — which is what lets `[Int]` skip the runtime's
  loop entirely.
- **`lower/expr.rs`**: `ArrayNew` (allocate full, then store each element through
  `array_slot` at a constant in-range index — a fresh slot, so a plain store),
  `Index` (`array_slot` + load + copy-out-before-drop-base), `ArrayLen`, and
  `ArrayAppend` (walk the place to the handle's slot, evaluate the value, then
  `array_push_slot` + store — value before the push, as the VM orders it).
- **`lower/stmt.rs`**: `store_place` walks the whole path to the destination
  slot. A `Field` step is a GEP (a struct is an inline aggregate); an `Index`
  step *loads* the handle and asks the runtime for the element's bounds-checked
  address. Index expressions are evaluated left to right and before the value,
  per `IrPlace`'s contract.

A fresh array local slot holds the **null** handle, which `array_len` reads as
`0` and `array_free` treats as nothing to free — so a slot is reclaimable before
its first store, exactly as a `String`'s null handle is.
