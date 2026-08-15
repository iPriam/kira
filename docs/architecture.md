# Architecture

Crates live in `crates/`, organized into layers with no upward dependencies —
ever. Each crate's `lib.rs` states its layer in its first doc line, and that
line is the source of truth. A test-only upward reference belongs in
`[dev-dependencies]`, which is cargo's one legal cycle.

| Layer | Crates |
|---|---|
| 0 | `kira-core`, `kira-toolchain`, `kira-source`, `kira-diagnostics`, `kira-diagnostic-messages`, `kira-runtime-abi`, `kira-dynamic-ffi` |
| 1 | `kira-syntax-model`, `kira-lexer`, `kira-parser`, `kira-ksl-syntax-model`, `kira-ksl-parser` |
| 2 | `kira-semantics-model`, `kira-shader-model`, `kira-ksl-semantics`, `kira-semantics` |
| 3 | `kira-ir`, `kira-shader-ir`, `kira-hybrid-definition`, `kira-backend-api`, `kira-native-lib-definition` |
| 4 | `kira-glsl-backend`, `kira-wgsl-backend`, `kira-hlsl-backend`, `kira-msl-backend`, `kira-spirv-backend`, `kira-bytecode`, `kira-vm-runtime`, `kira-native-bridge`, `kira-hybrid-runtime`, `kira-debug`, `kira-llvm-backend` |
| 5 | `kira-manifest`, `kira-project`, `kira-package-manager`, `kira-build-definition`, `kira-main` (Rust embedding surface: staticlib/cdylib/rlib) |
| 6 | `kira-program-graph`, `kira-hybrid-main` (hybrid embedding surface: bytecode half plus a loaded native half) |
| 7 | `kira-build` (frontend driver, library builds, generated Rust wrapper crates) |
| 8 | `kira-profile`, `kira-linter`, `kira-doc`, `kira-app-generation`, `kira-live` |
| 9 | `kira-cli` (binary `kira`) |
| tests | `kira-export-consumer` (a Rust program consuming a Kira library, end to end, on each of the three engines) |
| runners | `kira-desktop-runner` (binary `kira-desktop-runner`) |
| tools | `kira-launcher` (binary `kira-launcher`, installed as `kira`), `kira-knvm` (binary `knvm`), `kira-devflow` (binary `devflow`) |

`kira-lsp` is the language-server surface over the salsa frontend, consuming the
same frontend the compiler does.

`kira-mcp` and `kira-lldb-mcp` are the two agent-facing servers, sharing their
JSON-RPC framing through `kira-mcp-protocol`. They are split by lifetime rather
than by subject: `kira-mcp` answers one compiler question per call, while a
debug session outlives the call that started it, so `kira-lldb-mcp` holds the
LLDB processes and the built targets its sessions are debugging. Neither is a
layer — both sit above `kira-cli`, and `kira-lldb-mcp` builds through it.

## The rules that decide where code goes

Model types and logic are split: shared vocabulary lives in `*-model` crates,
logic above them. A lower layer that must call upward gets a trait in an
interface crate — `kira-backend-api` is the pattern — implemented higher up.

`kira-core`, `kira-source`, and the `*-model` crates are frozen roots. Touching
one rebuilds the world, so a change there needs a stated reason.

The VM stays portable. `kira-vm-runtime` and everything below it take no
filesystem, process, thread, or dynamic-loading dependency and must compile for
`wasm32-unknown-unknown`; the VM consumes bytes and reaches the world only
through `HostCapabilities`. Native-only functionality — dynamic FFI, `dlopen` —
is feature-gated and lives in `kira-hybrid-runtime` and `kira-native-bridge`.

A runner consumes bundles, never compiler internals. The `.klbundle` is the
boundary: a runner reads a manifest plus payloads, with no dependency on
`kira-ir`, `kira-semantics`, or any backend. Building a bundle needs LLVM;
running one never does, which is what lets a runner ship to a machine without
it.

## Building

```sh
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The VM-hot crates (`kira-vm-runtime`, `kira-bytecode`) compile at `opt-level = 3`
even in the dev profile: a debug interpreter runs 4–11× slower, and the dev
snapshot is what `kira run` uses for interactive work.

`run`, `build`, and `check` take a `.kira` file or a package directory. Omit it
and they use the package directory you are standing in, so `kira check` inside
an app is `kira check .`. A directory holding no `package.kira` is refused by
name.
