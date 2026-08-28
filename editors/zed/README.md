<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraZedExtensionDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraZedExtensionLight.png">
  <img alt="Kira Zed Extension" src="Images/KiraZedExtensionDark.png">
</picture>

# Kira Zed Extension

Zed editor extension for the [Kira programming language](https://github.com/kira-lang-com/kira).
Provides syntax highlighting, bracket matching, indentation rules, and the
Kira language server's diagnostics and navigation features for `.kira` files.

## Features

- Syntax highlighting via Tree-sitter
- Bracket matching and auto-close
- Indentation rules
- Comment toggling (`//`)
- Diagnostics via `kira-language-server`
- Hover information and prefix-filtered completion
- Go to definition and go to declaration, including imported files

### Language server features

The language server uses the same frontend as `kira check`. Errors and warnings
appear inline as you type. It also provides:

- **Hover** for a resolved name, showing the declaration's source line.
- **Completion** for declarations in the current file and resolved imported
  declarations. Completions are filtered by the identifier prefix and replace
  only that prefix; `.` is a completion trigger character.
- **Go to definition** and **go to declaration** for resolved references.
  Kira has no separate declaration/header form, so both requests land on the
  same definition. Cross-file references use the imported file's URI.

The server advertises full-document synchronization. Each open buffer is
analyzed from its current text, while imports are resolved from the document's
filesystem location. Only the requested document receives pushed diagnostics;
unopened files are not proactively published. Features not advertised here —
including rename, formatting, references, and symbol search — receive the
standard LSP `MethodNotFound` response rather than a guessed answer.

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

Highlighting works without the server. Diagnostics and language-server features
depend on `kira-language-server`. If the server cannot be found, Zed surfaces
an error naming the install command; the rest of the extension keeps working.

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

This extension connects to the Tree-sitter grammar in the
[Kira monorepo](https://github.com/kira-lang-com/kira) at a pinned commit SHA
for reproducible installs. The grammar path and revision are declared in
`extension.toml`; Zed fetches and builds that pinned grammar when it installs
the extension. A local WASM toolchain is not required for a normal install.

## Updating the Grammar

When the Tree-sitter grammar is updated:

1. Update the grammar and its corpus under `editors/tree-sitter` in the Kira
   monorepo.
2. Run the Tree-sitter corpus tests and commit the grammar change.
3. Update `[grammars.kira].rev` in `extension.toml` to that reachable commit
   SHA.
4. Install the dev extension in Zed and verify highlighting before publishing.

## Troubleshooting

**"Failed to compile grammar 'kira'"** — verify that the repository URL, the
commit revision, and `path = "editors/tree-sitter"` in `extension.toml` point
at the same reachable grammar commit. For a local grammar change, run the
Tree-sitter corpus tests and install the extension again.

**Wrong folder selected** — Install the folder containing `extension.toml`,
not the `kira-tree-sitter` repository.

**"kira-language-server not found in PATH"** — the server is not installed, or
Zed cannot see it. Run `cargo install --path crates/kira-lsp` from a kira-rusty
checkout and restart Zed. If it is installed and the error persists, Zed is
resolving a PATH without `~/.cargo/bin` — set an explicit `binary.path` under
the `kira-lsp` key as shown in [Language server settings](#language-server-settings).

**No diagnostics, no error either** — highlighting comes from the grammar and
works with no server at all, so a quiet editor is not proof the server is
running. Check Zed's language server logs (`zed: open log`). If diagnostics,
hover, completion, or navigation are all absent, verify that the server is
installed and that Zed can find the configured binary.

## Compatibility

Built against `zed_extension_api` version `0.7.0`. Modifying the Rust extension
code requires a Rust toolchain.

## License

MIT
