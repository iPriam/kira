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

## Cross compilation

`kira build --target <arch-os-abi>` emits and links for a machine that is not
this one — `kira build --target aarch64-linux-gnu`, for instance. The triple is
the same `arch-os-abi` spelling a manifest's `nativeLibraries` rows are keyed by,
so a package's `[target.aarch64-linux-gnu]` archives are the ones such a build
selects, and a `package.kira` may name the triple as its `buildTarget` instead of
repeating it on every command line.

`--target` implies the LLVM backend: the interpreter runs bytecode here, not on
the other machine. `run`, `test`, and `debug` refuse a cross target by name for
the same reason — each of them has to start the program it built — and a `live`
session is always this machine's, since a runner reloads the app in its own
process. Artifacts go under `.kira-build/<toolchain-triple>/`, so a host build
and a cross build of one program never overwrite each other.

Three settings go with it:

- `--sysroot <dir>`, or the `KIRA_SYSROOT` environment variable, names the
  directory holding the target machine's headers and libraries — the one with
  `usr/include/stdio.h` under it. There is no compiled-in default, because a
  sysroot is a property of the machine doing the building rather than of the
  package: Fedora's `sysroot-aarch64-fc41-glibc` unpacks to
  `/usr/aarch64-redhat-linux/sys-root/fc41`, Debian's `libc6-dev-arm64-cross`
  installs into `/usr/aarch64-linux-gnu`, and a container image puts it wherever
  it likes. `rpm -ql <sysroot-package>` and `dpkg -L <cross-libc-package>` are
  how to find the one a given machine has.
- `--relocation-model pic|static` chooses between an ordinary
  position-independent executable and an absolutely-addressed, non-PIE image. It
  defaults to `pic` and applies only to a `--target` build; host executables link
  position-independent everywhere Kira runs.
- `--linkage dynamic|static` chooses whether the program still needs a loader to
  start. It defaults to `dynamic` and also applies only to a `--target` build.

The last two are separate flags because they are separate decisions, made by
different halves of the build: the relocation model is what the code generator
bakes into the objects, and the linkage is what the linker makes of them. A
program can be absolutely addressed and still name `/lib/ld-linux-aarch64.so.1`
as its interpreter, which on a machine that has no such file the kernel refuses
before `main` — so the program never runs to report it. A userland with no
dynamic loader wants both `static` answers; a statically linked PIE is an
ordinary thing to want and asks for only the second.

Three things have to be in place before a cross build can link:

- The managed LLVM must carry the target's code generator. `llvm-metadata.toml`
  pins `X86;AArch64;WebAssembly` so every published bundle can emit for every
  machine Kira publishes for, but a bundle installed before that pin carries only
  the one belonging to the runner that built it. Such a build reports the missing
  generator by name, and `knvm install-llvm --force` replaces the bundle.
- The managed LLVM must ship `lld`. clang selects a linker rather than containing
  one, and the only linker a machine has by default builds for that machine — a
  Windows host handed an ELF object to a PE linker and got `unrecognised
  emulation mode: elf_x86_64`. `scripts/llvm/build-llvm.*` build the `lld`
  project for this, and a bundle without it refuses the cross link by name.
- `libkira_native_bridge.a` — the Rust `staticlib` every native Kira program
  links — must exist built for the target, since the Rust standard library inside
  it is that machine's code. Build it with
  `cargo build -p kira-native-bridge --target <toolchain-triple>`; the compiler
  finds cargo's output beside its own, and `KIRA_NATIVE_BRIDGE_<TARGET>` names it
  outright when it lives somewhere else.
