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

`kirac live` builds a program into a `.klbundle`, serves it over a loopback
socket, and runs it on a runner client. `--watch` reloads on every save: a
bytecode-only edit swaps into the running process, and anything the process
cannot take in place relaunches it and says why.

```sh
kirac live app.kira                             # the VM half
kirac live --backend hybrid app.kira            # both halves
kirac live --watch app.kira                     # reload on every save
```

[docs/live.md](docs/live.md) covers the bundle format, the `live.*` event
vocabulary, the two reload tiers and how one is chosen, what is watched, and
what a hot patch does and does not preserve.

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
