# Architecture internals

Repository-internal rules split out of the old `docs/architecture.md` when that
tree was folded into `sites/docs`. The public half is
`sites/docs/content/docs/appendix/compiler-architecture/index.mdx`.

## Layer declaration

Each crate's `lib.rs` states its layer in its first doc line, and that line is
the source of truth. A test-only upward reference belongs in
`[dev-dependencies]`, which is cargo's one legal cycle.

## Where code goes

Split model types from logic: shared vocabulary lives in `*-model` crates, logic
above them.

Give a lower layer that must call upward a trait in an interface crate.
`kira-backend-api` is the pattern, implemented higher up.

Put business logic as low in the graph as it can reasonably live. Keep binaries
as leaves.

## Frozen roots

`kira-core`, `kira-source`, and the `*-model` crates are frozen roots. Touching
one rebuilds the world, so a change there needs a stated reason in the commit
body.

## Build profile

`kira-vm-runtime` and `kira-bytecode` compile at `opt-level = 3` even in the dev
profile: a debug interpreter runs 4 to 11 times slower, and the dev snapshot is
what `kira run` uses for interactive work.

## String operations implementation trail

`kira-semantics/src/strings.rs` type-checks the surface. The four operations
lower through `HirExpr`/`IrExpr` to the VM's `StringCharAt`, `StringSubstring`,
`StringIndexOf`, and `StringOf` opcodes, and to the `kira_rt_str_char_at`,
`kira_rt_str_substring`, `kira_rt_str_index_of`, and `kira_rt_str_of_*` runtime
helpers natively. Parity is held by
`crates/kira-cli/tests/backend_parity/strings.rs` and `.../macros.rs`.

## Struct seam implementation trail

The VM traps `VmError::StructAtSeam` when a struct reaches a `CallNative` with
no tree built for it, and reports `VmError::MissingSeamWriteback` when a
written-through parameter does not come back. Both mean a module and a manifest
that disagree, never a program that merely type-checked.

The manifest carries the mode per parameter (`Ownership::BorrowMut`), generated
from the same IR both halves are compiled from.

`NewStruct` carries its own arity and `StoreField` carries the whole field path,
so a nested write mutates in place.

## Macro pass implementation trail

`kira-macros` is called from `kira_semantics::expanded`, the one query between
reading a file and parsing it. `expanded` orchestrates `kira_macros::scan`
(what one file declares), the program-wide environment built from the
macro-declaring files, and `kira_macros::expand_one` (one file's fixpoint).

Each piece is a salsa query keyed on an interned file, so a dependency that has
not changed is expanded once per session rather than once per compilation. The
only cross-file dependency is a `kind { wrapper }` macro; when a program
declares one, the files carrying its templates join the key.

`@Derive(Copy)` is answered by `kira-semantics`, not this crate: copyability is
a question about a whole reachable shape rather than about syntax.

Coverage: `crates/kira-macros/src`,
`crates/kira-semantics/src/tests/copyable.rs`, and
`crates/kira-cli/tests/backend_parity/macros.rs`.

## knvm transport audit

The GitHub transport has never been executed against the live API. Feed parsing,
by-tag parsing, channel mapping, tag stripping, asset selection, and sidecar
reading are unit-tested against canned API JSON, but no test opens a network
connection. That covers `install`, `install-llvm`, `self-update`,
`list --remote`, and both bootstrap scripts equally: every one is exercised only
down to the transport.

Windows in the release workflow is the first execution the install path gets on
that platform; a verify-step failure there is a finding, not noise.
`install.ps1` has never run at all.

The bootstrap scripts are documented as living at
`https://kira-lang.com/install.sh`, which nothing serves yet. Until that URL
exists the bootstrap is a file in this repo rather than a route a user can take.

Still deferred: provisioning the libffi bundle from knvm, and a
`knvm self-update` that can roll back to the version it replaced.

## Live session trust boundary

The live server enforces ordering and ownership of `live.*` milestones, not
honesty: a runner that downloads a bundle, discards it, and reports each
milestone in order will be believed. The evidence a session is real comes from
the app's own behavior, which is why the end-to-end tests assert on its stdout.
