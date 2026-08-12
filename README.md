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

The implementation is young and says so. The compiler, debugger, instruction
profiler, FFI inspector, linter, documentation model, package/dependency,
shader, and live-reload tools are wired through the real pipeline. `kira help`
lists every available command.

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

`knvm binstall` builds the compiler optimized, installs it under
`~/.kira/toolchains/dev/<version>/`, and points the `kira` launcher at it. The
launcher dispatches to the *installed* toolchain, so a `cargo build` alone does
not change what `kira` runs — rerun `knvm binstall` after compiler changes, or
invoke `./target/release/kira` directly. `knvm binstall --debug` stages the
unoptimized build, for debugging the compiler itself: it compiles faster and
then compiles every project it builds several times slower.

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

Twenty-two runnable packages live in `examples/`, each exercising one part of the
language: `hello`, `fib`, `arrays`, `structs`, `classes`, `enums`, `generics`,
`closures`, `ownership`, `match`, `switch`, `loops`, `strings`, `widths`,
`bitwise`, `aliases`, `imports`, `attempt`, `library`, `foundation`, `ffi`, and
`networking`.

```bash
kira run examples/generics
kira check examples/library
kira doc examples/library > api.md
kira run --backend llvm examples/ownership
```

## Debugging

`kira debug` exposes the same entrypoint on all three execution backends:

```bash
kira debug --backend vm --break main examples/hello
kira debug --backend hybrid --batch --break main examples/ffi
kira debug --backend llvm --batch --break main examples/hello
```

The VM and hybrid sessions support `continue`, `step`, `break`, `backtrace`,
`locals`, `stack`, `disassemble`, and `quit`, and print the mapped Kira source
line at each stop. The LLVM session launches the real LLDB executable, loads
the generated DWARF/native debug data (CodeView/PDB on MSVC), and captures the
stopped thread's frame, source window, variables, backtrace, registers, and
target CPU instructions; set `KIRA_LLDB` when LLDB is not on `PATH`.

The VM can also be hosted entirely by LLDB:

```bash
kira debug --backend vm --lldb --batch --break main:2 examples/hello
```

This exports `kira_vm_debug_probe` from the host. LLDB stops on that native
frame, and its register arguments carry the current VM function id, bytecode
PC, opcode, call depth, stack depth, and live local/operand-stack pointers.
Use `register read` and CPU disassembly in LLDB; on x86-64 the first two
location fields are the platform argument registers (`rcx`/`rdx` on Windows,
`rdi`/`rsi` on System V). The Kira VM remains the execution engine—the probe
is its LLDB ABI, not a translation of bytecode into native code. Every probe
also publishes the exported `KIRA_VM_DEBUG_STATE` C-shaped snapshot. The
VM host also maintains the exported `KIRA_VM_DEBUG_TEXT` mirror. Batch LLDB
reads that mirror at the stop, so decoded local slots, operand-stack values,
instruction bytes, and the VM backtrace describe the exact stop. On LLDB builds
with a stable command interpreter, type `continue` to reach the next requested
stop, then inspect the raw state yourself with `memory read --size 4 --count 16 &KIRA_VM_DEBUG_STATE`
(the address form is needed by Swift LLDB on Windows,
which types the exported C global as `void*`). For repeated stops on the Swift
Windows build, use the DAP command below.

For a real multi-stop VM session on Windows, use LLDB's Debug Adapter Protocol
frontend. It drives the same native probe but avoids the Swift command
interpreter's second-`continue` crash:

```bash
kira debug --backend vm --lldb-dap --dap-continues 2 --break calculateTax:5 examples/debug-lab/buggy.kira
```

The DAP launch verifies the native breakpoint, stops in one LLDB-owned process,
and reads the decoded VM state with DAP `evaluate` plus `readMemory`. The
`--dap-continues` value controls additional real `continue` requests; an IDE or
other DAP client can launch the same host and arguments when it should own the
interactive session. The exported probe/state symbols are the runtime ABI.

Use `--lldb` on a hybrid debug run to place the VM host and native shared
library in one LLDB session:

```bash
kira debug --backend hybrid --lldb --batch --break fast examples/ffi
```

The VM half still reports Kira bytecode stops through its portable observer,
while LLDB resolves `@Native`/runtime symbols in the loaded library and
provides native frames, registers, DWARF source data, and CPU disassembly. A
VM-only run without `--lldb` uses the portable Kira debugger; with `--lldb`,
the probe described above is the LLDB stop surface. On some Windows LLDB builds
hybrid DLL unwinding is unreliable, so the hybrid launcher omits only the
automatic backtrace query while retaining frame, register, source, and
disassembly inspection.

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
[docs/macros.md](docs/macros.md). The standalone `kira shader` verb builds and
validates these targets directly.

## Packages and toolchains

A package is declared by a `package.kira` manifest authored in Kira itself.
`kira sync` resolves the manifests and writes `kira.lock`; a build that finds a
stale lockfile regenerates it and says so. `kira add` and `kira remove` edit
dependency declarations; `kira sync` then resolves the edited graph and writes
a fresh lockfile. Updating registry pins, packaging, and manifest migration
remain separate commands listed by `kira help all`.

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
