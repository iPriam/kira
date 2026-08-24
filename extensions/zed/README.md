<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraZedExtensionDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraZedExtensionLight.png">
  <img alt="Kira Zed Extension" src="Images/KiraZedExtensionDark.png">
</picture>

# Kira Zed Extension

Zed editor extension for the [Kira programming language](https://github.com/kira-lang-com/kira).
Provides syntax highlighting, bracket matching, indentation rules, and
diagnostics for `.kira` files.

## Features

- Syntax highlighting via Tree-sitter
- Bracket matching and auto-close
- Indentation rules
- Comment toggling (`//`)
- Diagnostics via `kira-language-server`

### Diagnostics only

The language server surface is **diagnostics only**. Errors and warnings appear
inline as you type, and that is the whole of it — there is **no hover, no
completion, no goto-definition, no rename, no formatting, and no symbol
search**.

The server advertises exactly one capability (full-document sync), so an editor
that reads the handshake knows not to ask for the rest. Should something ask
anyway, it gets a `MethodNotFound` error back rather than silence or a wrong
answer. Everything else the editor does for `.kira` files comes from the
Tree-sitter grammar, not the server.

**Diagnostics are per-file, not per-project.** Each open `.kira` buffer is
analyzed on its own: the server has no notion of a project, a manifest, or
another file, because the language has no imports or modules yet. Nothing is
reported for a file you do not have open, and no error can ever point across
files.

## Installation

### Prerequisite: the language server

Diagnostics require the `kira-language-server` binary, which is **not bundled**
with this extension and is not published to crates.io. Build and install it
from a [kira-rusty](https://github.com/kira-lang-com/kira-rusty) checkout:

```sh
cargo install --path crates/kira-lsp
```

That puts `kira-language-server` in `~/.cargo/bin`. Restart Zed afterwards so
it picks up the new binary.

Highlighting works without the server — only diagnostics depend on it. If the
server cannot be found, Zed surfaces an error naming the install command; the
rest of the extension keeps working.

### Dev Install (local)

1. Clone this repository
2. Open Zed → Extensions → Install Dev Extension
3. Select the folder containing `extension.toml`

Make sure you select the extension folder, not the `kira-tree-sitter` repo.

## Language server settings

The extension resolves the server in this order:

1. An explicit path in your Zed settings (below)
2. `kira-language-server` on the worktree's PATH
3. Otherwise, an error naming the `cargo install` command above

To point at a specific build — one out of a `target/release` directory, or a
binary kept off PATH — override it in Zed's `settings.json` under the
`kira-lsp` key:

```jsonc
{
  "lsp": {
    "kira-lsp": {
      "binary": {
        "path": "/absolute/path/to/kira-language-server",
        "arguments": []
      }
    }
  }
}
```

The path must be absolute. `arguments` should stay empty: the server takes no
CLI arguments and speaks LSP over stdio only. An explicit path always wins over
PATH discovery.

Each field stands on its own. `arguments` and `env` apply to whichever binary
ends up being run, so setting them without a `path` overrides how the
discovered server is launched rather than being ignored. Omitting `env` hands
the server the worktree's shell environment, which is what you want unless you
have a reason not to.

### Publishing

The extension will be published to the Zed marketplace. Once available, search
"Kira" in Zed's extension browser.

## How it Works

This extension connects to the [kira-tree-sitter](https://github.com/kira-lang-com/kira-tree-sitter)
grammar at a pinned commit SHA for reproducible installs. A prebuilt
`grammars/kira.wasm` is bundled with the extension — no local WASM build
tooling required on install.

## Updating the Grammar

When `kira-tree-sitter` is updated:

1. Build the new WASM: `npx tree-sitter build --wasm` in the grammar repo
2. Copy the output to `grammars/kira.wasm` in this repo
3. Update `[grammars.kira].rev` in `extension.toml` to the new commit SHA
4. Commit and push

## Troubleshooting

**"Failed to compile grammar 'kira'"** — Zed is trying to compile the grammar
locally without the bundled WASM. Make sure `grammars/kira.wasm` is present.
If rebuilding, ensure `emcc`, `docker`, or `podman` is available.

**Wrong folder selected** — Install the folder containing `extension.toml`,
not the `kira-tree-sitter` repository.

**"kira-language-server not found in PATH"** — the server is not installed, or
Zed cannot see it. Run `cargo install --path crates/kira-lsp` from a kira-rusty
checkout and restart Zed. If it is installed and the error persists, Zed is
resolving a PATH without `~/.cargo/bin` — set an explicit `binary.path` under
the `kira-lsp` key as shown in [Language server settings](#language-server-settings).

**No diagnostics, no error either** — highlighting comes from the grammar and
works with no server at all, so a quiet editor is not proof the server is
running. Check Zed's language server logs (`zed: open log`). Note that hover,
completion, and goto-definition are unimplemented by design, so their absence
is not a symptom of anything: see [Diagnostics only](#diagnostics-only).

## Compatibility

Built against `zed_extension_api` version `0.7.0`. Modifying the Rust extension
code requires a Rust toolchain.

## License

MIT