# Capture cells

A `var` a closure captures moves into a **share-counted heap box** — the first
genuinely shared mutable reference in the language, and the first construct
capable of forming a cycle. Everything else here has value semantics: a struct
copies deeply, an array's block is shared only until a writer buys its own, an
enum is never written through. So this is a new shape, not an existing one
reused.

The box is the **enum box**. `KiraEnum` is `{ tag, payload_kind, payload,
shares }`; a cell needs everything but the tag, so a cell is one of those with
the tag at zero. The share bump generated code emits inline, the payload-kind
switch that decides what a release reclaims, and the free path are all the ones
enums already proved — no second ownership implementation to keep in step, and
no new `#[repr(C)]` type. On the VM it is `Object::Cell { payload, shares }`,
which needs no kinds at all because the VM holds a `Value`.

## The rewrite

```text
  var total = 0          ->  let total = CellNew(0)        Type::Cell(Int)
  … total …              ->  CellGet(total)
  total = e              ->  CellSet(total, e)
  total[i] = e           ->  let t = CellGet(total)
                             t[i] = e
                             CellSet(total, t)
  { … total … }          ->  the closure captures the cell, by handle
```

`Type::Cell(CellId)` is not surface: no annotation resolves to one and no
written expression produces one. It exists between the declaration that mints it
and the reads and writes that go through it.

## The seven invariants, and where each lives

**A cell outlives its frame.** It is heap storage with a share count, so a
closure that escapes through a trailing callback, an `@FFI.Callback` pointer, or
a `Task` keeps its box alive by holding a share of it. `HirStmt::Let` of a
`CellNew` allocates per *execution*, so a `var` declared inside a loop is a
fresh binding each turn and a closure made on one turn keeps that turn's
storage.

**Capture analysis over-approximates.** `closures::captures` walks the function
body before anything is analyzed and collects every identifier written inside a
closure literal, at any depth. A `var` whose *name* is in that set is boxed,
whatever the name turns out to mean. Boxing has to happen at the declaration —
the capture that needs it is discovered later, after every read of the binding
has been lowered — so the question has to be answered from syntax, and answered
early. The asymmetry decides the direction: an extra box costs an allocation, a
missing box is a closure and a frame writing different storage.

Filtering by name is not the coarsest answer available. Boxing every `var` in
any function containing a closure needs no walk at all, and would put a heap box
behind every loop counter in every function with a callback in it.

**`cell_set` is one primitive.** `Instruction::CellSet`, `IrStmt::CellSet`, and
`kira_rt_cell_set` each release the old payload and store the new one with
nothing in between, and nothing is ever handed a pointer into the payload slot.
A split path traps between the release and the store and leaves a freed handle
in the box for good.

**`cell_get` returns an owned value.** The VM's `Heap::cell_get` runs
`copy_value` on the payload; `kira_rt_cell_get` clones a string, enum, or erased
handle on the way out. A borrowing read lets a write through another holder free
the payload while a caller still has it — and a cell exists precisely so that
other holders exist.

**Writing an aggregate through a cell is read-modify-write.** `Analyzer::cell_place`
hoists `let t = CellGet(cell)` ahead of the statement, roots the place at `t`,
and defers `CellSet(cell, t)` until after it. The read hands back a second
handle on one block; the element write runs the ordinary uniqueness check, sees
the block shared, and buys elements of its own; the store-back is what makes
that new block the one the cell holds. Sharing the cell is intended; sharing the
array inside it with unrelated holders is not.

A statement mentioning one cell twice gets one temporary, not two — otherwise
two writes through one storage would look like writes to different storage and
`places_overlap` would stop refusing them.

**Cells are refused at the C seam and at `Any` erasure.** `Type::assignable_to`
refuses the erasure, `foreign_type_of` and `check_export_type` the seams, and
`kira-build`'s `tag` the hybrid manifest. A hold taken outside this runtime is a
hold nothing releases.

**Cycles leak.** A cell holding a closure that captures the same cell cannot be
collected by share counts. The box and everything it reaches leak — memory-safe,
never freed twice, never freed early, never reclaimed. Collecting it needs a
tracing collector this runtime does not have, so it is documented in
`Heap::free_cell` and `kira-native-bridge`'s `cells` module rather than defended
against.

## What was appended

Three opcodes, `NEW_CELL` (`0x5f`), `CELL_GET` (`0x60`), and `CELL_SET`
(`0x61`), the last two carrying a `u16` slot. Seven `kira_rt_cell_*` symbols:
`new`, `new_aggregate`, `get`, `get_aggregate`, `set`, `set_aggregate`, `free`.
Every one is an addition; no existing signature changed and
`RUNTIME_ABI_VERSION` did not move.

The get and set forms name a **slot** rather than take a handle off the stack.
Every cell the compiler mints lives in a local — a boxed `var` is a local of its
frame, and a captured one is copied out of the closure's representation struct
into a local by the lifted body's prologue — so naming the slot lets a read
borrow the handle instead of taking a share of it and dropping it again, the
same reason `ARRAY_GET_LOCAL` exists.

## Where it is proven

`crates/kira-cli/tests/backend_parity/captured_vars.rs` is the bar: eleven cases
on vm, llvm, and hybrid, each looping its interaction 100–200 times so a
share-count imbalance crashes rather than leaking quietly, and each printing an
accumulated total so one missed write in two hundred is a different number.

Parity alone does not cover the store-back. Removing it leaves all three
backends agreeing on the *wrong* answer, because all three run the same desugar
— which is why the two aggregate cases pin concrete expected values rather than
only comparing backends.
