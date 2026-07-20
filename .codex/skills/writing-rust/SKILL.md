---
name: writing-rust
description: "The Rust laws for this workspace: no lifetimes in model types, unsafe fenced with SAFETY comments, newtypes over primitives, interned strings, open C enums as transparent newtypes, typed thiserror errors, no unwrap/expect outside tests, no #[allow] escapes, no panicking stubs, docs on every pub item, and hot-path allocation rules. Read before writing, editing or judging any .rs file."
---

# Writing Rust here

Treat these rules as mechanical on purpose: they hold no matter which model or
contributor is editing.

## Types and memory

- **Keep lifetimes out of model types.** Give AST/HIR/IR model types no
  lifetime parameters — follow the index/arena pattern as law (ids into arenas,
  never `&'a` references), and keep `Rc`/`RefCell` out of AST/HIR/IR. Reach for
  `la-arena`/`bumpalo` as the sanctioned tools. Express intra-tree references as
  typed index newtypes. **Scope:** read this as covering *model* types — the
  trees and their nodes. Never stretch it to ban lifetimes on short-lived
  workers that borrow their input (`Parser<'a>`, `Lexer<'a>`, `Codegen<'a>`,
  `Vm<'h>`, a decoder's `Reader<'a>`) or on seam vocabulary that borrows by
  design (`NativeArg<'a>`). Recognize `kira-core`'s `Symbol` as the mechanism
  that keeps the model side clean: model types store interned handles instead of
  borrowed slices.
- **Intern every name.** Spell names and identifiers as `kira_core::Symbol`;
  reserve `String` in a model type for genuinely owned free text (raw literals,
  messages). Never reach for `&'static str` to carry user data.
- **Own by default.** Let containers own their data (`Vec`, `Box<[T]>`,
  `String`); introduce `bumpalo` arenas only where profiling shows the win, per
  phase, never globally. Make a type that runs something own what it runs.
- **Wrap primitives in newtypes.** Model ids, offsets, and handles as
  `#[repr(...)]` newtypes (`SourceId(u32)`, `Span{start,len}`), never as bare
  `u32`/`usize` passed around. Defer to an existing contract's spelling over a
  new newtype: the seam already speaks `function_id: u32`, so its mirror does
  too.
- **Never model an open C enum as a Rust enum.** Express a byte foreign code can
  write as a transparent newtype with associated consts (`BridgeValueTag`) —
  treat an out-of-range discriminant in a Rust `enum` as the UB it is. Reserve
  `enum(u8)`-style Rust enums with explicit discriminants for closed, Kira-owned
  tags.

## Fence the unsafe

Confine `unsafe` to designated core crates (runtime, FFI, LLVM bindings); keep
it out of model and orchestration crates entirely. Carry a `// SAFETY:`
comment on every block (clippy's `undocumented_unsafe_blocks` enforces it),
and name the invariant in a doc comment on every unsafe-bearing field. Anchor
invariants on the type inside the fence.

## Correctness and hygiene

- **Fix the code, never the lint.** Add no `#[allow(...)]` and loosen no
  workspace lint.
- **Ship no panicking stub.** Leave no
  `todo!()`/`unimplemented!()`/`panic!` as a placeholder in committed code —
  record unported behavior as a doc-comment `TODO(port)` on a typed stub. Write
  **no `unwrap`/`expect` outside `#[cfg(test)]`**, including for a condition you
  believe cannot happen: a library never gets to end its caller's process. Raise
  an id-space overflow as a typed error (`CompileError::TooManyStrings`,
  `InternerFull`, `SourceMapFull` set the precedent), and restructure a lookup
  that "can't fail" so it genuinely *can't* — index by a total function instead
  of searching and unwrapping.
- **Type every error.** Return `Result` from a fallible function with a
  `thiserror` enum the crate owns; reject `Box<dyn Error>` and stringly errors
  across crate boundaries. Route user-facing diagnostics through
  `kira-diagnostics`, never `eprintln!`.
- **Document every pub item.** Give one line minimum, stating what it is.
- **Keep tests beside the code.** Put unit tests in `#[cfg(test)]` next to what
  they test and layout tests next to `#[repr(C)]` types. Change anything
  `#[repr(C)]` only together with a layout test in the same file.
- **Never trade a panic for a wrong answer.** When a search-and-unwrap becomes
  an index, make the ordering invariant real (private fields, one constructor)
  and pin it with a test — an out-of-place row that used to panic must not
  become a silently wrong lookup.

## Performance

- **Treat the interpreter as special.** Compile `kira-vm-runtime`/`kira-bytecode`
  at `opt-level = 3` even in dev (workspace profile — never remove it; a debug
  interpreter runs 4–11× slower). Keep dispatch match-in-loop until `become`
  stabilizes; skip NaN-boxing (measured ±5%, not worth it).
- **Allocate nothing on a hot path.** Keep per-op heap allocation and `format!`
  off interpreter and drop paths' success paths, and read env vars once at init
  — remember the per-drop `getenv` regression as the cautionary tale.
- **Never optimize speculatively elsewhere.** Outside designated hot crates,
  write the clear version and let profiling promote it. Never bolt a deep clone
  onto a hot structure to make an ownership problem go away — that is how a
  19GB leak ships.
