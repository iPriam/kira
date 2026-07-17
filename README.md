# kira-rusty

The Kira compiler and runtime in Rust — this is *the* Kira implementation, a
brand-new codebase. Kira has been rewritten from the ground up several times
over its life; this Rust repo is where it comes home.

Scaffolding phase: one crate per compiler/runtime concern, wired into a
layered dependency graph. The frontend, IR, VM, and backends are being
designed fresh from the language corpus, not carried over from any prior
implementation.

## Workspace layout

Crates live in `crates/`, organized into layers with no upward dependencies.

| Layer | Crates |
|---|---|
| 0 | `kira-core`, `kira-toolchain`, `kira-source`, `kira-diagnostics`, `kira-diagnostic-messages`, `kira-runtime-abi`, `kira-dynamic-ffi` |
| 1 | `kira-syntax-model`, `kira-lexer`, `kira-parser`, `kira-ksl-syntax-model`, `kira-ksl-parser` |
| 2 | `kira-semantics-model`, `kira-shader-model`, `kira-ksl-semantics`, `kira-semantics` |
| 3 | `kira-ir`, `kira-shader-ir`, `kira-hybrid-definition`, `kira-backend-api`, `kira-native-lib-definition` |
| 4 | `kira-glsl-backend`, `kira-wgsl-backend`, `kira-hlsl-backend`, `kira-msl-backend`, `kira-spirv-backend`, `kira-bytecode`, `kira-vm-runtime`, `kira-native-bridge`, `kira-hybrid-runtime`, `kira-debug`, `kira-llvm-backend`, `kira-wasm-runtime` |
| 5 | `kira-manifest`, `kira-project`, `kira-package-manager`, `kira-build-definition` |
| 6 | `kira-program-graph` |
| 7 | `kira-build` |
| 8 | `kira-instruments`, `kira-linter`, `kira-doc`, `kira-app-generation`, `kira-live` |
| 9 | `kira-cli` (binary `kirac`) |
| 10 | `kira-main` (C ABI facade: staticlib/cdylib/rlib) |
| runners | `kira-desktop-runner` (binary `kira-desktop-runner`) |
| tools | `kira-bootstrapper` (binary `kira`), `kira-devflow` (binary `devflow`) |

`kira-lsp` is the language-server surface over the salsa frontend.

## Building

```sh
cargo build
cargo clippy --workspace
```

The VM-hot crates (`kira-vm-runtime`, `kira-bytecode`) are compiled with
`opt-level = 3` even in the dev profile: a debug interpreter is 4–11× slower,
and the dev snapshot is what `kira run` uses for interactive work.

## Live sessions

`kirac live <file>` builds the program into a `.klbundle`, serves it over a
loopback socket, and starts a runner client that downloads it, loads it, links
it, and starts it:

```sh
kirac live examples/strings/strings.kira                 # the VM half
kirac live --backend hybrid path/to/app.kira             # both halves
kirac live --watch app.kira                              # reload on every save
kirac live --watch --quit-after 30s app.kira             # bounded
```

The bundle is the runner's whole world. A `.klbundle` is a manifest (`KLB1`)
beside a flat payload directory, each payload named by its SHA-256 content hash;
a runner consumes that and never reaches into a compiler data structure, which
is what lets the compiler's internals change without breaking every runner.
Payloads are verified against the manifest on arrival, so a runner holds the
bytes the build produced or it holds an error.

Sessions report the `live.*` vocabulary as milestones actually occur, and each
milestone belongs to the end that can know it. The server observes that a runner
connected and that bytes went out; only the runner can report that they loaded,
and the server rejects a runner that claims otherwise. A session is ready only
once every required milestone has arrived in order — a runner cannot assert its
way past a bundle it never loaded.

`kira-desktop-runner` is the runner client that ships today. It hosts both a VM
bytecode entrypoint and a hybrid one, `dlopen`ing the native half for the
latter: running a bundle needs no LLVM, only building one does. It is headless,
which is why sessions stop at `live.entrypoint.started` rather than claiming
`live.frame.presented` — presenting a frame needs a window and a swapchain, and
kira-graphics owns those, not this repo.

Every runner id parses. One this build has no client for — `ios`, `android`, and
the rest — reports precisely that rather than failing as an unknown command.

### Reload

`--watch` rebuilds on every save and gets the change into the running app. There
are two tiers, and the tier is chosen by what actually changed:

- **hot patch** — the rebuilt native library is byte-for-byte the loaded one, so
  the edit was a bytecode-only edit whatever the source looked like. The bytecode
  swaps into the process that is already running: same process, same loaded
  library, nothing re-`dlopen`ed.
- **relaunch** — anything else. The runner is replaced, and the reason is
  reported: the process has the old library's code mapped and native state
  holding pointers into it, so a swap would leave the two halves disagreeing
  about what the other one is.

The rule is byte identity, not a source diff, which is why payloads are named by
a collision-resistant hash. Nothing degrades quietly: a bundle that cannot be
hot-patched says so and says why, rather than relaunching silently and leaving
someone wondering where their state went. `KIRA_LIVE_NO_HOTPATCH=1` turns tier 1
off entirely, so a session can run with the swap path removed rather than merely
unused.

A save that changes nothing does nothing. A save that does not compile prints its
diagnostics and leaves the running app alone — killing a working app over a
half-typed line would make watching worse than not watching.

What survives a hot patch today is the process and its loaded library. *App
state* surviving is the eventual promise and it is not testable yet: the language
has no globals and no closures, so there is no state that outlives a call to
preserve. The two rejection conditions that protect such state — a struct or enum
whose layout changed, and a live closure whose function changed signature — are
not checked, because neither can happen yet. They are not skipped; there is
nothing to skip. `kira-live`'s `reload::decide` is where they land when those
features do.

## Editor support

`kira-lsp` builds `kira-language-server`, the language-server binary editors
talk to. Install it from a checkout of this repo:

```sh
cargo install --path crates/kira-lsp
```

It lands in `~/.cargo/bin`. The server speaks LSP over stdio, takes no CLI
arguments, and serves **diagnostics only** — it handles `initialize`,
`didOpen`, `didChange`, `didClose`, and publishes diagnostics. It advertises
full-document sync and nothing else, so a client knows not to ask for more;
anything that asks anyway gets `MethodNotFound` rather than a wrong answer.
Hover, completion, and goto-definition are not implemented yet.

Analysis is **per-file**: each open document is analyzed alone, because the
language has no imports or modules yet. There are no project-wide diagnostics,
and nothing is reported for a file that is not open.

The server runs the same salsa frontend `kirac check` does, so an editor
squiggle and a command-line error are the same computation rather than two
implementations that agree until they do not.

### Zed

The [Kira Zed extension](https://github.com/kira-lang-com/kira-zed-extension)
provides syntax highlighting via Tree-sitter plus diagnostics from the server
above. Install the server first — the extension does not bundle it and, since
`kira-lsp` is unpublished, cannot download it. The extension finds the binary
on the worktree's PATH; to point at a specific build instead, set an explicit
path in Zed's `settings.json`:

```jsonc
{
  "lsp": {
    "kira-lsp": {
      "binary": { "path": "/absolute/path/to/kira-language-server" }
    }
  }
}
```

Restart Zed after installing or replacing the binary.
