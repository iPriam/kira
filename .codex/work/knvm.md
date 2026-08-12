# knvm - decision record

Built 2026-07-19 as `crates/kira-knvm` (lib plus bin `knvm`), together with the
`kira` launcher's dispatch in `kira-launcher`. What they do and how they
behave is [docs/knvm.md](../../docs/knvm.md); this note holds the decisions
behind them and what is left.

`install` is real: fetch, stage, unpack, validate, `rename` into
`~/.kira/toolchains/<channel>/<version>/`, write `current.toml`. `list`, `use`,
and `uninstall` operate on what is on disk. The launcher reads the selection
and hands the process over to the selected `kira`, so an install is now
reachable as `kira` rather than by naming `bin/kira` by hand.

## Two corrections to the original sketch

The bundled Foundation is marked by **`foundation/package.kira`**, not the
`kira.toml` this note first drew. `kira-toolchain`'s `bundled_discovery` is the
authority, and install validates against it.

`current.toml` is **not inert**, as the earlier survey of the repo claimed.
Discovery rule 3 already reads it to find Foundation for a consumer that is not
`kira` itself, so writing it is what makes an installed toolchain usable, not
merely selectable.

## Decisions settled

**A crate in this workspace, not a script.** `kira-knvm` is a standalone leaf
at the binary layer like `kira-launcher`, depending only on `kira-toolchain`
plus workspace externals already present. The `curl | sh` bootstrap that
installs knvm itself is release infrastructure needing published binaries and a
hosted URL, neither of which exists; deferred rather than sketched.

**One layout definition.** Every path knvm touches comes from `kira-toolchain`.
The install orchestration - subprocesses and filesystem mutation - stays in the
tool crate rather than moving down to layer 0, where it would drag process
spawning into `kira-llvm-backend`'s dependency tree for no consumer.

**`KIRA_HOME` in layer 0.** One deliberate `kira-toolchain` change, following
the `KIRA_FOUNDATION_HOME` / `KIRA_LLVM_HOME` precedent. It is what lets every
install and launcher test run against a temp root - passed on a spawned child's
environment, never `set_var` in-process - and it reaches `current.toml` and
discovery rule 3 without either knowing about it. Parsing of `current.toml`
also moved from hand-rolled string slicing to the `toml` crate the crate
already depended on; the schema and `to_toml` output are unchanged.

**Transport behind a trait, `curl` behind that.** `ReleaseSource` has two
operations; everything downstream of it is one code path, so
`DirectoryReleaseSource` lets tests exercise the shipped pipeline with only the
transport substituted. GitHub transport is one isolated `curl` subprocess
rather than an HTTP crate, keeping external dependencies frozen. Swapping in a
native client is a change to one function.

**Channels are both a namespace and a feed.** `release` is the default, per the
rustup and nvm norm. If pre-1.0 reality means only dev builds are published,
flipping the default is one line in `cli.rs`.

**`llvm/` and `libffi/` are knvm's eventually, untouched now.** They are
version-independent siblings shared across toolchain versions, so a toolchain
install must not mutate them. A test asserts witness files in both survive.
Provisioning them is a separate command driven by `llvm_metadata::pinned`.

**Launcher exit code 2 is redefined.** It used to mean "unimplemented"; it now
means "the launcher could not dispatch", and every such failure names
`knvm install latest`. Past a successful dispatch there is no launcher process
left to translate anything: on unix the image is replaced, so the exit status
and signal disposition are the toolchain binary's by construction.

**Removing the selected toolchain clears the selection.** `uninstall` deletes
`current.toml` and warns rather than repointing at a surviving version, which
would silently change which compiler a user runs. For the same reason `use`
refuses a version whose tree has lost its `bin/kira`: a selection the launcher
cannot dispatch is a failure worth reporting at selection time.

## Known-unverified, not assumed-good

The GitHub feed path has never run. Parsing and asset selection are unit-tested
against canned JSON, but nothing is published at `kira-lang-com/kira`, so the
artifact naming and tag conventions are guesses behind a trait. Downloads get
no checksum or signature check. Windows is compile-checked only, for both the
install path and the launcher's spawn-and-wait dispatch. The version comparator
is not semver - a trailing prerelease tag sorts above the release, pinned by a
test asserting the real behavior rather than papered over. And a `SIGKILL`
mid-install can leave an inert directory under `.staging/` that nothing prunes.

## The Foundation seam is proven, and rule 3 is not what proves it

`crates/kira-cli/tests/end_to_end/installed_toolchain.rs` now drives the whole
seam: `kira_knvm::install` lays a real release down under a throwaway
`KIRA_HOME`, and the compiler out of that tree runs `import Foundation`.

Writing it corrected the assumption above. A compiler *inside* an installed
toolchain never reads `current.toml` - `foundation/` sits beside its own `bin/`
and the shipped rule answers first. Rule 3 decides only for a compiler outside
any toolchain, so the two cases are separate tests, and the negative half
deletes `current.toml` to prove the selection is what resolved the import.

The trap the first draft fell into is worth naming: a test that installs a
toolchain and runs the compiler from it *looks* like a rule 3 test and is not
one. It passes with `current.toml` deleted.

## binstall: the developer loop is closed

`knvm binstall` builds the enclosing checkout optimized (`--debug` for the
unoptimized build), stages the compiler and the checkout's `foundation/` into the
release-install pipeline, and lands it on `dev` as the selected toolchain. A
second run replaces the tree — never "already installed", which for a dev
build would mean silently stale. This is what makes knvm usable day to day
against this repo before any release exists.

## Still to build

Archive checksum verification; `knvm install-llvm`; listing versions available
remotely rather than installed; knvm self-update; per-project toolchain
pinning; and the `curl | sh` bootstrap.
