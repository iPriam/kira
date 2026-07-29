---
name: where-to-change
description: "Crate map and layering rules for this workspace: which crate owns lexer/parser/semantics/IR/bytecode/VM/LLVM/hybrid/shader/CLI/toolchain changes, the layer-0..8 no-upward-dependency DAG, the model/logic split, and which crates are frozen. Read when it is unclear which crate a change belongs in, or before adding a crate dependency."
---

# Where to change things

Crate dependencies form a strict DAG — no upward dependencies, ever. Each
crate's `lib.rs` states its layer in its first doc line; that line is the
source of truth — keep it accurate when a crate moves. Put a test-only
upward reference in `[dev-dependencies]` (cargo's only legal cycle).

## Layers

- **0 — vocabulary:** `kira-core` (`Symbol`, interner), `kira-source`
  (`Span`, `SourceId`, `SourceMap`), `kira-runtime-abi` (`BridgeValue`,
  `Execution`, `Ownership`, `HostCapabilities`), `kira-diagnostics`,
  `kira-diagnostic-messages`, `kira-toolchain`, `kira-dynamic-ffi`
- **1 — syntax:** `kira-lexer`, `kira-parser`, `kira-syntax-model`,
  `kira-ksl-parser`, `kira-ksl-syntax-model`
- **2 — semantics:** `kira-semantics`, `kira-semantics-model`,
  `kira-ksl-semantics`, `kira-shader-model`
- **3 — IR and interfaces:** `kira-ir`, `kira-shader-ir`,
  `kira-backend-api`, `kira-hybrid-definition`, `kira-native-lib-definition`
- **4 — engines and backends:** `kira-bytecode`, `kira-vm-runtime`,
  `kira-llvm-backend`, `kira-native-bridge`, `kira-hybrid-runtime`,
  `kira-debug`, and the shader backends
  (`kira-msl-backend`, `kira-glsl-backend`, `kira-hlsl-backend`,
  `kira-wgsl-backend`, `kira-spirv-backend`)
- **5–8 — project and tools:** `kira-manifest`, `kira-project`,
  `kira-package-manager`, `kira-build-definition`, `kira-main` (5),
  `kira-program-graph`
  (6), `kira-build` (7), `kira-app-generation`, `kira-doc`,
  `kira-instruments`, `kira-linter`, `kira-live` (8)
- **top — binaries:** `kira-cli` (`kira`), `kira-launcher` (`kira-launcher`,
  installed onto PATH as `kira`), `kira-devflow` (`devflow`), `kira-lsp`,
  `kira-desktop-runner`. Leaves — keep logic lower.

## Runners consume bundles, never compiler internals

Put the live session model — bundle format, protocol, server, client — in
`kira-live` (8), and a runner client in its own binary crate above it
(`kira-desktop-runner` is the pattern). The `.klbundle` is the boundary: a
runner reads a manifest plus payloads and nothing else, so give it no dependency
on `kira-ir`, `kira-semantics`, or any backend. A runner needing a compiler type
means the bundle is missing a field — add the field.

Building a bundle needs LLVM; running one never does. Keep it that way: a runner
that linked the LLVM backend could not ship to a machine without it.

## Rules that decide the crate

- **Split model from logic.** Keep shared vocabulary types in `*-model` crates
  and logic above them. Give a lower layer that must call upward a trait in an
  interface crate — follow `kira-backend-api` as the pattern, implemented
  higher up.
- **Keep root crates thin and frozen.** `kira-core`, `kira-source`, and the
  model crates (`kira-syntax-model`, `kira-semantics-model`,
  `kira-shader-model`) rebuild the world when touched — a change there needs
  a reason stated in the commit, and churning logic never moves down into
  them.
- **Define each contract once.** Anchor `Span` in `kira-source` and the runtime
  value ABI in `kira-runtime-abi`. Re-export or alias from other crates — never
  redefine.
- **Model selection as a structured enum, never a string.** Route backend and
  platform choices through a real type (`BackendMode::{VmBytecode, LlvmNative,
  Hybrid}`, `Execution`, `RunnerId`), never a matched-on `&str`. A stringly
  branch cannot be exhaustively checked, so a new backend compiles clean and
  silently falls through.
- **Re-export flat.** Have each crate's `lib.rs` re-export its public types
  flat (`kira_manifest::ProjectManifest`, not a deep module path). On renaming
  a `pub` item, fix every consumer in the same change.
- **Keep heavy generics out of low crates.** Recognize that monomorphization
  cost lands in every downstream crate. Give layer boundaries concrete types or
  `dyn Trait`, and keep generic helpers crate-private.
- **Preserve the VM as a portable core.** Keep `kira-vm-runtime` and every crate
  below it free of filesystem, process, thread, or dynamic-loading call, and must
  compile for `wasm32-unknown-unknown`. The VM consumes bytes and reaches the
  world only through `HostCapabilities`. Native-only functionality (dynamic
  FFI, `dlopen`) is feature-gated and lives outside the portable core —
  `kira-hybrid-runtime` and `kira-native-bridge` are where it belongs.
- **Put the embedding surface in `kira-main`.** Route anything a *consumer* of a
  Kira library needs — loading an artifact, checking it against the wrapper
  generated for it, instantiating it with a host, calling an export by name,
  releasing a handle — through `kira-main` (5), which sits above
  `kira-vm-runtime` because it embeds it. Keep it Rust: the C facade is v2
  growth of the same crate, and every C signature is append-only forever. The
  default `StdoutHost` lives here too, so `kira-cli` uses it rather than
  defining a second one.
- **Build the frontend as salsa queries.** Express parsing and semantics as
  queries: no hidden global state, no interior-mutability caches smuggled
  into analysis. The parser is error-resilient — it always produces a tree
  plus diagnostics, never bails on the first error — and every node carries
  spans, because the LSP consumes the same frontend the compiler does.
- **Treat dependencies as frozen.** Draw external crates only from
  `[workspace.dependencies]` with unified features; adding one is a
  deliberate root-level change with a stated reason. No parser generators, no
  chumsky — the lexer and parser are hand-written. LLVM goes through
  `llvm-sys`, never `inkwell`.

## Out of scope

- **Graphics.** This repo does not render: kira-graphics owns
  Metal/Sokol/Vulkan/D3D12. The surface here ends at shader codegen and the
  FFI/native bridge kira-graphics hangs off — which makes dynamic FFI and
  autobind critical path, not tail work.
- **Emscripten for the compiler.** Kira *apps* targeting Web keep the emcc
  subprocess pipeline; the compiler itself, if ever browser-hosted, targets
  `wasm32-unknown-unknown`. No rustc-emscripten linkage.
