---
name: writing-rust
description: "The Rust laws for this workspace: no lifetimes in model types, unsafe fenced with SAFETY comments, newtypes over primitives, interned strings, open C enums as transparent newtypes, typed thiserror errors, no unwrap/expect outside tests, no #[allow] escapes, no panicking stubs, docs on every pub item, and hot-path allocation rules. Read before writing, editing or judging any .rs file."
---

# Writing Rust here

## Types and memory

- **Keep lifetimes out of model types.** Give AST/HIR/IR types no lifetime
  parameters: ids into arenas (`la-arena`, `bumpalo`), never `&'a`, no
  `Rc`/`RefCell`. Express intra-tree references as typed index newtypes. This
  covers *model* types only. Never stretch it to short-lived workers that borrow
  their input (`Parser<'a>`, `Vm<'h>`) or seam vocabulary that borrows by design
  (`NativeArg<'a>`).
- **Intern every name.** Spell names as `kira_core::Symbol`; reserve `String`
  for genuinely owned free text. Never carry user data in `&'static str`.
- **Own by default.** Let containers own (`Vec`, `Box<[T]>`, `String`).
  Introduce arenas only where profiling shows the win, per phase, never
  globally. A type that runs something owns what it runs.
- **Wrap primitives in newtypes.** Model ids, offsets and handles as
  `#[repr(...)]` newtypes (`SourceId(u32)`), never bare `u32`/`usize`. Defer to
  an existing contract's spelling over a new newtype.
- **Never model an open C enum as a Rust enum.** Express a byte foreign code
  writes as a transparent newtype with associated consts. An out-of-range
  discriminant in a Rust `enum` is UB. Reserve `enum(u8)`-style Rust enums with
  explicit discriminants for closed, Kira-owned tags.

## Unsafe

Confine `unsafe` to core crates (runtime, FFI, LLVM bindings) and keep it out
of model and orchestration crates. Carry a `// SAFETY:` comment on every block,
and name the invariant in a doc comment on every unsafe-bearing field.

## Correctness and hygiene

- **Fix the code, never the lint.** Add no `#[allow(...)]`; loosen no workspace
  lint.
- **Ship no panicking stub.** No `todo!()`/`unimplemented!()`/`panic!` as a
  placeholder; record unfinished behavior as a doc-comment `TODO` on a typed
  stub. Write no `unwrap`/`expect` outside `#[cfg(test)]`, including for a
  condition that cannot happen. Raise an id-space overflow as a typed error
  (`InternerFull` sets the precedent), and restructure a lookup that "can't
  fail" so it genuinely can't: index by a total function rather than searching
  and unwrapping.
- **Never trade a panic for a wrong answer.** When a search-and-unwrap becomes
  an index, make the ordering invariant real (private fields, one constructor)
  and pin it with a test. A row out of place must not become a silent lookup.
- **Type every error.** Return `Result` with a `thiserror` enum the crate owns;
  reject `Box<dyn Error>` and stringly errors across crate boundaries. Route
  user-facing diagnostics through `kira-diagnostics`, never `eprintln!`.
- **Document every pub item**, one line minimum.
- **Keep tests beside the code**, in `#[cfg(test)]`. Put layout tests next to
  `#[repr(C)]` types and change either only together.

## Performance

- **Treat the interpreter as special.** Keep `kira-vm-runtime`/`kira-bytecode`
  at `opt-level = 3` even in dev; a debug interpreter runs 4-11x slower. Keep
  dispatch match-in-loop until `become` stabilizes; skip NaN-boxing.
- **Allocate nothing on a hot path.** Keep per-op allocation and `format!` off
  interpreter and drop success paths; read env vars once at init.
- **Never optimize speculatively elsewhere.** Outside the hot crates above,
  write the clear version and let profiling promote it. Never bolt a deep clone onto a hot structure to make an
  ownership problem go away: that is how a 19GB leak ships.
