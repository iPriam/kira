# Ownership

Kira now has an explicit ownership-oriented call model.

## Implemented now

- Kira owns by default.
- Plain parameters consume named non-trivial values.
- `borrow Type` parameters are read-only, non-consuming borrows.
- `borrow mut Type` parameters are non-consuming mutable borrows and require a mutable caller binding.
- `move expr` is parsed and enforced for named non-trivial ownership transfer.
- Trivial values like `Int`, `Float`, `Bool`, `CString`, and `RawPtr` still pass by value without `move`.
- Use-after-move diagnostics are emitted for later local uses.
- Borrowed parameters cannot be moved by the callee.
- Borrowed return types are parsed but still rejected until returned-borrow lifetime validation exists.
- Ownership metadata is preserved in semantic function headers, HIR, IR, bytecode, and LLVM monomorphization data.

## Current syntax

- `function inspect(mesh: borrow Mesh) -> Int`
- `function update(mesh: borrow mut Mesh)`
- `function consume(mesh: Mesh)`
- `function transfer(mesh: move Mesh)`
- `let result = consume(move mesh)`

## Current limits

- Non-trivial `copy expr` is reserved, but clone semantics are not implemented yet.
- Returned borrows are reserved, but lifetime validation is not implemented yet.
- Field-sensitive partial moves are not implemented yet.
- Closure escape borrow analysis is not implemented yet.
- LLVM ownership metadata is carried through the pipeline, but borrow-aware native ABI lowering such as `readonly` or `noalias` is not implemented yet.

## `any` examples

The docs use `any`-style examples instead of placeholder `T` examples.

- Planned container-style examples should look like `Array(any)`, `borrow any`, `borrow mut any`, `move any`, `copy any`, and `shared any`.
- Today, the implemented executable `any` surface is still construct-qualified: use `any Widget`, not angle-bracket placeholder syntax.
