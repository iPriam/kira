<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraBannerDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraBannerLight.png">
  <img alt="Kira" src="Images/KiraBannerDark.png">
</picture>

# Kira

Kira is a Zig-hosted compiler and toolchain for a systems-oriented language that can run through a VM, emit LLVM/native executables, or split work across a hybrid runtime/native boundary. The repository includes the compiler pipeline, CLI, package tooling, managed LLVM setup, native C-ABI interop, and KSL shader parsing/validation with GLSL output.

## Why Kira is interesting

- One source pipeline lowers `.kira` programs to VM bytecode, LLVM/native output, or hybrid bytecode plus a native library.
- `@Runtime` and `@Native` mark execution boundaries for mixed programs instead of forcing separate source worlds.
- Native library manifests are static-linking-first, C-ABI-focused, and can generate Kira bindings from headers.
- The language surface is covered by corpus tests across VM, LLVM, and hybrid backends where those paths apply.
- The `kira` CLI includes run/check/build workflows, package-management commands, project scaffolding, and managed LLVM fetching.
- KSL is a sibling shader language with dedicated parsing, semantic validation, reflection, and GLSL 330 output.

## A small Kira program

```kira
@Main
function main() {
    let greeting = "hello from Kira"
    var total = 40 + 2

    print(greeting)
    print(total)
    return
}
```

Kira examples use `@Main` to choose the entrypoint, `function` declarations for callable code, `let` for immutable locals, `var` for mutable locals, and `print(...)` for simple output. See the executable boundary in [docs/language_inventory.md](docs/language_inventory.md) for the current implemented surface.

## Quick start

Build the toolchain and install the launcher:

```bash
zig build
zig build install
kira --help
```

`zig build install` installs the PATH-facing launcher into `zig-out/bin/` by default and installs the active toolchain under `~/.kira/toolchains/<channel>/<version>/`. `zig build install-kirac` is also available for the same managed toolchain plus launcher flow.

GitHub release archives now ship the `kira` launcher separately from the managed toolchain payload. On first run, the release launcher downloads the matching `kira-toolchain-<platform>` archive into `~/.kira/toolchains/release/<version>/` and activates it automatically. LLVM remains a separate `kira fetch-llvm` step for native and hybrid workflows.

On Windows, run the launcher directly or add it to the current PowerShell session:

```powershell
.\zig-out\bin\kira.exe --help
$env:Path = "$PWD\zig-out\bin;$env:Path"
```

Run the checked-in examples:

```bash
kira run examples/hello
kira fetch-llvm
kira run --backend llvm examples/hello
kira run --backend hybrid examples/hybrid_roundtrip
```

Run the repo tests when validating code changes:

```bash
zig build test
```

LLVM and hybrid commands need a discoverable LLVM toolchain. The easiest path is `kira fetch-llvm`; advanced users can set `KIRA_LLVM_HOME` instead. See [docs/llvm_toolchain.md](docs/llvm_toolchain.md).

## Example gallery

The runnable sample index lives in [examples/README.md](examples/README.md). Useful starting points:

- `examples/hello` — basic VM/LLVM/hybrid executable example.
- `examples/arithmetic` — simple functions, locals, arithmetic, and structs.
- `examples/imports_demo`, `examples/report_pipeline`, `examples/geometry_story`, and `examples/status_board` — broader language-facing examples.
- `examples/hybrid_roundtrip` — hybrid runtime/native boundary roundtrip.
- `examples/callbacks` and `examples/callbacks_chain` — native callbacks and callback state.
- `examples/sokol_triangle` and `examples/sokol_runtime_entry` — Sokol/OpenGL native interop proofs using local `NativeLibs/` manifests.
- `examples/shaders` — `.ksl` shader examples compiled by the dedicated shader pipeline.

Try a few directly:

```bash
kira run examples/hello
kira run --backend llvm examples/callbacks
kira run --backend hybrid examples/sokol_triangle
kira check examples/sokol_runtime_entry
kira shader check examples/shaders/textured_quad.ksl
kira shader build examples/shaders/lit_surface.ksl
```

## Execution model

```text
.kira source
  -> frontend
  -> IR
  -> VM bytecode
   / LLVM native object + executable
   / hybrid bytecode + native shared library
```

- **VM** is the default path for quick local execution and bytecode output.
- **LLVM/native** lowers shared IR through LLVM and links a host-native executable.
- **Hybrid** compiles `@Runtime` functions to bytecode and `@Native` functions to a native shared library, then runs both in one host process through explicit bridge/trampoline calls.

The package layering and full pipeline are documented in [docs/architecture.md](docs/architecture.md) and [docs/package_graph.md](docs/package_graph.md).

## Native interop

Kira's current FFI path is intentionally narrow and practical: C ABI, static linking first, per-library TOML manifests, generated bindings as normal Kira source, direct extern calls from LLVM/native code, callback function pointers, and hybrid argument/result marshalling.

A project lists native libraries in its manifest, and each library owns its header paths, binding-generation configuration, source files, target archives, and linker details. The Sokol and callbacks examples show this shape in practice under local `NativeLibs/` directories.

Learn the manifest format and current limits in [docs/native_libraries.md](docs/native_libraries.md).

## KSL shaders

KSL is Kira's dedicated shader language surface. It is parsed and validated by a sibling pipeline, not by the executable `.kira` frontend. Today it can validate shader modules, emit reflection JSON, and lower graphics shaders to GLSL 330 for the repo's current Sokol/OpenGL path. Compute shaders are part of the source and semantic model, but the current GLSL 330 backend rejects them intentionally.

```bash
kira shader check examples/shaders/textured_quad.ksl
kira shader ast examples/shaders/textured_quad.ksl
kira shader build examples/shaders/textured_quad.ksl
kira shader build
```

With no explicit file, `kira shader build` discovers top-level PascalCase shader entry files under `Shaders/` in the current project and writes outputs to `generated/Shaders/`. See [docs/ksl.md](docs/ksl.md).

## Packages and toolchains

New projects use `kira.toml`; legacy `project.toml` is still discovered for existing examples. The package manager is source-only and lockfile-backed, with registry, path, and pinned git dependencies.

Common commands:

```bash
kira new DemoApp generated/DemoApp
kira new --lib GraphicsKit generated/GraphicsKit
kira sync
kira add FrostUI
kira add --git https://github.com/Sunlight-Horizon/GameKit.git --rev <commit> GameKit
kira package pack
```

Library roots are checkable and buildable, but execution belongs to app/example targets: `kira run .` on a library reports `KCL020`, and `kira live .` reports `KCL021`. For bounded smoke tests, `kira run <example> --quit-after 5s` and `kira live <example> --quit-after 5s` automatically shut down graphical/runtime sessions instead of leaving windows or child processes open. Targets that are valid for check/build/run but not live-bundle compatible are rejected before runner launch with `KCL031`.

LLVM discovery is explicit:

1. `KIRA_LLVM_HOME`
2. the managed install from `kira fetch-llvm`
3. older repo-managed fallback paths, if present

Command details are in [docs/commands.md](docs/commands.md), package-management details are in [docs/package_management.md](docs/package_management.md), and LLVM bundle details are in [docs/llvm_toolchain.md](docs/llvm_toolchain.md).

## Developing Kira

For compiler and CLI work, use Zig's build runner while iterating:

```bash
zig build
zig build run -- run examples/hello
zig build test
```

Run `zig fmt` on changed Zig files before finishing code changes. Do not hand-edit generated output under `generated/`, `.zig-cache/`, `zig-out/`, or `.kira/`.

## License

Kira is licensed under Apache 2.0 with the Kira Runtime Library Exception. See [LICENSE](LICENSE) for the full text, including the exception that covers ordinary runtime/library portions incorporated into resulting products built with Kira.

## Documentation

- [Architecture](docs/architecture.md)
- [Package graph](docs/package_graph.md)
- [Commands](docs/commands.md)
- [Language inventory](docs/language_inventory.md)
- [Native libraries](docs/native_libraries.md)
- [KSL shaders](docs/ksl.md)
- [LLVM toolchain](docs/llvm_toolchain.md)
- [Package management](docs/package_management.md)
- [Examples](examples/README.md)
