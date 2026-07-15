# kira-rusty

The Kira compiler and runtime in Rust — a port of
[kira-zig](https://github.com/kira-lang-com/kira). Kira's implementation
lineage: Rust → Swift → Zig → Rust (this repo brings it home).

Scaffold only for now: one crate per kira-zig package with the dependency
graph preserved; logic migration follows.

## Workspace layout

Crates live in `crates/`, one per kira-zig package (`kira_foo` → `kira-foo`).
Two Zig packages were merged instead of ported as crates: `kira_log` is a
module in `kira-core`, and `kira_llvm_toolchain_layout` is a module in
`kira-toolchain`.

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
| tools | `kira-bootstrapper` (binary `kira`), `kira-devflow` (binary `devflow`) |

## Building

```sh
cargo build
cargo clippy --workspace
```

The VM-hot crates (`kira-vm-runtime`, `kira-bytecode`) are compiled with
`opt-level = 3` even in dev profile, mirroring kira-zig's
ReleaseFast-in-Debug interpreter rule.
