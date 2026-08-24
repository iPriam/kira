<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraTreeSitterDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraTreeSitterLight.png">
  <img alt="Kira Tree Sitter" src="Images/KiraTreeSitterDark.png">
</picture>

# Kira Tree-sitter

Tree-sitter grammar for the [Kira programming language](https://github.com/kira-lang-com/kira).
Provides parsing and syntax highlighting support for editors that use Tree-sitter, primarily Zed.

## Contents

- `grammar.js` — Tree-sitter grammar definition
- `src/parser.c`, `src/grammar.json`, `src/node-types.json` — generated artifacts
- `tree-sitter.json` — metadata, scope `source.kira`

## Building

### Prerequisites

To regenerate the parser from `grammar.js`:
```bash
npx tree-sitter generate
```

To build the WASM grammar (requires one of: `emcc`, `docker`, or `podman` on PATH):
```bash
npx tree-sitter build --wasm
```

This outputs `tree-sitter-kira.wasm` in the repo root.

## Updating the Grammar

1. Edit `grammar.js`
2. Run `npx tree-sitter generate` to regenerate `src/`
3. Run `npx tree-sitter build --wasm` to rebuild the WASM artifact
4. Commit all generated files including `src/parser.c`

## Usage in Zed

Reference this grammar in your Zed extension's `extension.toml`:
```toml
[grammars.kira]
repository = "https://github.com/kira-lang-com/kira-tree-sitter"
rev = "<pinned commit SHA>"
```

Always pin to a specific commit SHA for reproducible installs.

## License

MIT