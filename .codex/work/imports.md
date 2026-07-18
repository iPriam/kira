# Imports: file-scoped modules

The frontend is multi-file now. One `SyntaxTree` spans every file of a program
and records which file each item came from; `import` binds a namespace root in
exactly one file; and the CLI, the LSP, and every backend see the same flat
program they saw before.

## What the oracle actually pins

The corpus case is `tests-kik/corpus/imports/`, and the failure cases are
`tests-kik/fail-corpus/graph/` with four fixtures. Reading them settled three
things that shaped the design.

**The file-scope gate is a *package* gate.**
`packages/kira_semantics/src/lower_shared.zig:117` — `importedSymbolVisible`
looks a name up in `imported_symbol_owner`, which maps a name to the
**packages** that own it, and returns `true` when the name has no owner entry.
Root-package declarations have none. So within one package, every top-level
name is visible bare in every file, imported or not. The four `file_scoped_*`
fixtures are all about `Foundation`, which is a *dependency package*, not a
sibling module.

That is why an import here binds a qualifier and pulls a file into the program,
and gates nothing. Building a same-package visibility gate would have rejected
programs the oracle accepts.

**Cycles are legal.** `packages/kira_program_graph/src/builder.zig:174` —
`appendProgramGraph` returns early on a `visited` hit. There is no cycle
diagnostic anywhere in the oracle. The task asked for one; it is **refused**,
and `mutually_importing_modules_are_accepted` is the test that pins the
refusal. Inventing a rejection the reference does not have is a worse bug than
the one it would catch.

**The diagnostic codes are the oracle's.** `KSEM032` unresolved import
(`builder.zig:258`), `KSEM027` unresolved namespace root
(`lower_exprs_call_resolution.zig:356`). `KSEM012` (bare name) and `KSEM078`
(bare type) are the cross-package half of the gate and have nothing to fire on
here, because there are no packages.

## Design

**Loading is injected; resolution is not.** `kira-semantics` is layer 2 and
must keep compiling for `wasm32-unknown-unknown`, so it cannot open a file.
`kira-program-graph` (layer 6, previously an empty stub) walks the import graph
from the entry file, reads what it finds, and hands the texts to the frontend
as a salsa input field. The frontend decides which import binds which name and
reports the ones that bind nothing — with the span of the import, which only it
has.

A pleasant consequence: the wasm and semantics test suites build multi-module
programs **in memory**, with no temp directory, because module texts are an
input rather than a filesystem call.

**One tree, many files.** `SyntaxTree.items` and a new `item_sources` are
private and grown only by `push_item(source, item)`, so `items_with_source()`
is total by construction — the same private-fields-one-constructor pattern
`FnCtx.locals`/`ownership` already uses. `Analyzer.source` moves as the walk
proceeds and is what both a diagnostic's file and a qualified name's import set
are read from.

Modules are parsed **before** the entry file, dependencies first, because a
struct field may only name a struct declared earlier.

**`Root.name` is recognized in the analyzer, not the parser.** The parser
cannot tell `Support.hello()` from `point.length()`: both are
`<expr> . name ( args )`. The analyzer asks whether the receiver is a bare name
that is no local and that *this file* imported — a question that needs the
file-scoped table. A local of the same name wins, so importing a module never
makes a variable unusable.

A qualified **type** (`Support.Point`) is one interned symbol carrying the dot.
A dot cannot appear in an identifier, so a qualified spelling collides with
nothing a user declared, and `resolve_named_type` strips the qualifier after
checking the import.

## No desugar, and no backend work

There was nothing to desugar: an import is not a construct that produces code,
it is a question about scope. And there is no backend slice — the IR a
multi-file program lowers to is byte-identical to the one the same declarations
in one file produce. Every backend gets parity for free, which is what the
eight `backend_parity/imports.rs` cases and the six wasm ones check, rather than
assert.

## What is not here

- **No `Foundation`.** `import Foundation` reports `KSEM032`. There is no
  stdlib package in this repo to import, and shipping a stub that resolved to
  nothing would be fake success.
- **No cross-package gate** (`KSEM012`/`KSEM078` with an import hint). It needs
  packages, which need a manifest-driven dependency graph. The two codes stay
  free.
- **No qualified struct literal** (`Support.Point { x: 1 }`). The parser reads
  it as a field access; the qualified *type* spelling covers the annotation
  position, which is where the corpus uses it.
- **`Support.name` as a value** (not a call) is not resolved: there are no
  module-level constants to name.
- **The LSP reads modules from disk, not open buffers.** An unsaved edit in a
  module is not what its importer is checked against. Routing buffers in means
  the server owning a store keyed by module path; a right answer from a saved
  file beats a wrong one from a stale buffer.

## Files

- `kira-syntax-model`: `ast::ImportDecl`, `Item::Import`, `TokenKind::As`,
  `SyntaxTree::{push_item, items, items_with_source}`.
- `kira-parser`: `parse_files`, `parse_import`, dotted type names,
  `intern_text`.
- `kira-program-graph`: `load_modules` — the whole crate.
- `kira-semantics`: `imports.rs` (new), `ModuleSource`, `module_source_id`,
  `SourceProgram::modules`, per-item `source`, qualified call and type
  resolution. `typeck.rs` split to `typeck/calls.rs` (the ladder).
- `kira-cli`, `kira-lsp`: load modules, mirror them into the `SourceMap`.
