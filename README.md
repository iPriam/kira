<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraBannerDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraBannerLight.png">
  <img alt="Kira" src="Images/KiraBannerDark.png">
</picture>

# Kira

Kira is a compiler and toolchain for a systems-oriented language that runs the
same program three ways: on a bytecode VM, as an LLVM-compiled native
executable, or split across a hybrid runtime/native boundary. This repository is
the Rust implementation — the compiler pipeline, the VM, the LLVM and hybrid
backends, the C-ABI interop layer, and the KSL shader pipeline.

The implementation is young and says so. `kira run`, `check`, `build`, `sync`,
and `live` work; fifteen further verbs parse and report that they are not
implemented yet. `kira help all` lists them.

## Why Kira is interesting

One frontend lowers a `.kira` program to a single IR, and three backends consume
that same IR. A program's observable behavior is therefore comparable across
them by construction, and a parity test suite checks exactly that rather than
trusting it.

`@Runtime` and `@Native` mark where a function's body executes, so a mixed
program is one source world rather than two. The hybrid backend compiles the
runtime half to bytecode and the native half to a shared library, then runs both
in one process.

The VM is a portable core: it reaches the outside world only through a host
capability trait, takes no filesystem or process dependency, and compiles to
`wasm32-unknown-unknown`.

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

`@Main` chooses the entrypoint, `let` binds an immutable local, `var` a mutable
one. The language guide in [docs/language.md](docs/language.md) covers structs,
classes, enums, ownership, pattern matching, closures, and the rest of the
surface with runnable examples.

## Quick start

Build the workspace and install this checkout as your active toolchain:

```bash
cargo build --workspace
knvm binstall
kira --help
```

`knvm binstall` installs the built compiler under
`~/.kira/toolchains/dev/<version>/` and points the `kira` launcher at it. The
launcher dispatches to the *installed* toolchain, so a `cargo build` alone does
not change what `kira` runs — rerun `knvm binstall` after compiler changes, or
invoke `./target/debug/kira` directly.

Run the checked-in examples:

```bash
kira run examples/hello
kira run --backend llvm examples/hello
kira run --backend hybrid examples/ffi
```

The LLVM backend is a hard dependency of the build: its build script discovers a
managed LLVM bundle under `~/.kira/toolchains/llvm/<version>/<host>/`, or wherever
`KIRA_LLVM_HOME` points. Without one, nothing builds.

Provisioning it is knvm's job, and knvm links no LLVM — so it builds from a bare
checkout, before the bundle exists. That is the whole bootstrap:

```bash
cargo run -p kira-knvm -- install-llvm   # downloads the pinned bundle
cargo build --workspace                  # now the backend has an LLVM to link
```

`kira` itself never provisions LLVM: it links the backend, so a `kira` that could
fetch LLVM would have to be built before the bundle it installs.

## Examples

Twenty-one runnable packages live in `examples/`, each exercising one part of the
language: `hello`, `fib`, `arrays`, `structs`, `classes`, `enums`, `generics`,
`closures`, `ownership`, `match`, `switch`, `loops`, `strings`, `widths`,
`bitwise`, `aliases`, `imports`, `attempt`, `library`, `foundation`, and `ffi`.

```bash
kira run examples/generics
kira check examples/library
kira run --backend llvm examples/ownership
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

The **VM** is the default and the quickest path from source to output. The
**LLVM/native** backend lowers the same IR through LLVM and links a host-native
executable. The **hybrid** backend splits one program on its `@Runtime` and
`@Native` annotations and runs both halves in a single host process, marshalling
across the boundary.

Crates are organized into layers with no upward dependencies; the layout and the
rule are in [docs/architecture.md](docs/architecture.md).

## Native interop

The FFI path is C-ABI, static-linking-first, and driven by per-library manifests
that own their headers, sources, target archives, and linker details. Bindings
generate into ordinary Kira source. Callbacks cross in both directions, and the
hybrid host marshals arguments and results across the runtime/native seam.

See [docs/ffi.md](docs/ffi.md) for the manifest format and the current limits.

## KSL shaders

KSL is Kira's shader language, parsed and validated by a sibling pipeline rather
than the executable `.kira` frontend. Its crates —
`kira-ksl-parser`, `kira-ksl-semantics`, `kira-shader-ir`, and the MSL, WGSL,
GLSL 330, HLSL, and SPIR-V backends — are in this workspace, and a build
compiles every shader its program names for all five. SPIR-V is the one that
emits binary rather than source, and it travels in the artifact as hexadecimal,
eight characters per word. `ksl!` is no builtin: it is an ordinary `comptime
macro` the engine declares, over the one compile-time call the compiler owns,
`Ksl.compile(path, target)`. See
[docs/macros.md](docs/macros.md). The standalone `kira shader` verb is not
implemented yet.

## Packages and toolchains

A package is declared by a `package.kira` manifest authored in Kira itself.
`kira sync` resolves the manifests and writes `kira.lock`; a build that finds a
stale lockfile regenerates it and says so. Dependency commands — `add`,
`remove`, `update`, `package` — are planned, not built.

Toolchain management is `knvm`: `knvm install`, `knvm use`, `knvm list`,
`knvm binstall` for the current checkout. See [docs/knvm.md](docs/knvm.md).

## Developing Kira

```bash
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
```

All four gates run in CI with the managed LLVM provisioned first, so a green
local run means the same configuration CI checks. `cargo nextest` needs a
one-time install; the workspace notes in `AGENTS.md` carry the command.

## License

Kira is licensed under Apache 2.0 with the Kira Runtime Library Exception. See
[LICENSE](LICENSE) for the full text, including the exception covering ordinary
runtime and library portions incorporated into products built with Kira.

## Documentation

- [Language guide](docs/language.md)
- [Architecture](docs/architecture.md)
- [Foundation](docs/foundation.md)
- [Native interop](docs/ffi.md)
- [Macros](docs/macros.md)
- [Strings](docs/strings.md)
- [Structs](docs/structs.md)
- [Live sessions](docs/live.md)
- [Toolchain manager](docs/knvm.md)
