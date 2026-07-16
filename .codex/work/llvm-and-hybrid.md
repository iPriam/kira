# LLVM/native and hybrid

Orientation for the two non-VM backends. The VM path (`kira-bytecode` →
`kira-vm-runtime`) is not covered here.

## Status

- **LLVM/native: landed.** `kirac build|run --backend llvm` produces a real
  native executable.
- **Hybrid: landed.** `kirac build|run --backend hybrid` splits a program on its
  `@Runtime`/`@Native` annotations, emits both halves plus a manifest, and runs
  the bundle in `kira-hybrid-runtime` (the host). Both call directions work,
  including strings, traps, nesting, and a `@Native` entrypoint.

Parity is proven, not asserted: twenty-one differential tests plus every
example, identical stdout and exit status across **all three** backends
(`crates/kira-cli/tests/backend_parity.rs`).

`.codex/work/hybrid-handoff.md` carries the seam's ownership rules — the ones
that are a double free rather than a compile error when read wrong.

## Building it

The backend is feature-gated so the workspace builds and lints with no LLVM
installed. With a managed LLVM present:

```sh
LLVM_SYS_221_PREFIX=~/.kira/toolchains/llvm/22.1.4/aarch64-macos \
  cargo build -p kira-native-bridge -p kira-cli --features kira-cli/llvm
```

`-p kira-native-bridge` is not optional. `cargo build -p kira-cli` refreshes
that crate's rlib but **not** its staticlib, so the archive next to `kirac` can
be older than the compiler that links it. `cargo build --workspace` covers both.

## Contracts

Each is append-only and has a test that pins it. Change one only together with
the code that emits or reads it.

| Contract | Defined in | Shape |
|---|---|---|
| `kira_rt_*` | `kira-native-bridge/src/runtime.rs` | native runtime: print, strings, div-zero trap |
| `kira_rt_str_data` / `_len` | `kira-native-bridge/src/runtime.rs` | the host's only way to read a `KStr` — generated code never calls these |
| ABI marker | `kira-runtime-abi/src/lib.rs` | `RUNTIME_ABI_VERSION` / `RUNTIME_ABI_MARKER` |
| Host-resolved symbols | `kira-runtime-abi/src/lib.rs` | `HYBRID_HOST_SYMBOLS`: what the link forces in and the host `dlsym`s — one list, both sides |
| Host→VM entry | `kira-vm-runtime/src/interp.rs` | `Program::call` — the mirror of `HostCapabilities::call_native` |
| `BridgeValue` | `kira-runtime-abi/src/bridge.rs` | `{ u8 tag, [7]u8 reserved, u64 payload }`, 16 bytes |
| `Execution` | `kira-runtime-abi/src/execution.rs` | `Inherited` / `Runtime` / `Native` |
| `Ownership` | `kira-runtime-abi/src/ownership.rs` | `Owned` / `Borrow` / `BorrowMut` |
| `.khm` | `kira-hybrid-definition/src/manifest.rs` | `KHM1`: payload paths, entry, per-function engine + signature + symbol |
| Trampoline | `kira-llvm-backend/src/codegen/mod.rs` | `void kira_native_fn_<id>(const BridgeValue*, u32, BridgeValue*)` |
| Native→VM | `kira-native-bridge/src/hybrid.rs` | `kira_hybrid_call_runtime`, `kira_hybrid_install_runtime_invoker` |

## Decisions that look arbitrary and are not

- **A `String` crosses the native ABI as an opaque one-pointer handle**, never a
  `{ptr,len}` aggregate. LLVM IR is not ABI-aware — aggregate lowering is a
  frontend's job — so a backend that passes only pointer-sized scalars cannot
  disagree with the runtime's `extern "C"` about registers.
- **The native runtime is Rust, not C.** It formats with the same standard
  library the VM does, so `print` output is identical by construction rather
  than by reimplementation. This is what makes float formatting agree (`2.0`
  prints `2`); a C runtime would have to re-derive Rust's shortest-round-trip
  float formatting to match.
- **Native lowering mirrors the interpreter, not what a C compiler would do:**
  integer ops carry no `nsw`/`nuw` (they wrap, as the VM's `wrapping_*` do),
  `MIN / -1` is branched around rather than left as `sdiv` poison, and a zero
  divisor calls the trap helper instead of being undefined.
- **The VM performs no part of a native call.** It hands the embedder safe Rust
  values through `HostCapabilities::call_native`; the embedder does the
  marshalling. Teaching the VM to dlopen would cost the portable core, which
  must keep compiling for `wasm32-unknown-unknown`.
- **The host installs an invoker; the library leaves no undefined symbol.** So
  the shared library is self-contained and needs no `--export-dynamic` /
  `-undefined dynamic_lookup` arrangement with whatever loads it.
- **`CallNative` is its own opcode, not a flag checked by `Call`.** The compiler
  already knows which engine a callee is on, so the boundary costs an opcode and
  the ordinary path pays no branch.
- **A single-backend build ignores the annotations.** `--backend vm` compiles
  every function to bytecode and `--backend llvm` makes every function native:
  an execution boundary needs two engines, and these builds have one. That is
  what keeps the two backends agreeing on any program, annotated or not.
- **Args borrow, results own** at every crossing, in both directions. Kira's
  model is Rust's, so the question is Rust's question.

## Pitfalls

- **A hybrid library does not export what the host resolves, by default.** A
  linker pulls only *referenced* members out of an archive, and generated code
  references none of the symbols the host `dlsym`s: it never calls
  `kira_hybrid_install_runtime_invoker` (the host does), and it only references
  `kira_rt_str_new`/`_free`/`_data`/`_len` when the native half happens to use
  strings. This was confirmed by building it: a `@Native function double(n: Int)`
  produced a dylib exporting exactly two symbols, and the host failed on
  `kira_rt_str_new`. `link_shared_library` now passes `-Wl,-u,_<symbol>` (Mach-O)
  / `-Wl,--undefined=<symbol>` (ELF) for each of `HYBRID_HOST_SYMBOLS`, which
  pulls in exactly the defining member — deliberately narrower than
  `-force_load`/`--whole-archive`, which would drag the whole Rust standard
  library into every hybrid program.
- **The host must not link `kira-native-bridge`.** `kirac` already carries its
  own copy of every `kira_rt_*` symbol, so allocating a handle in one copy and
  freeing it in the other is a cross-allocator free. `kira-hybrid-runtime`
  therefore depends on the runtime crate *not at all* and resolves every symbol
  out of the loaded library; `libloading` opens `RTLD_LOCAL`, which is what keeps
  the two apart. Do not add that dependency back for convenience.
- **The runtime archive can be stale**, and a stale archive resolves every
  symbol by name — the mismatch used to be silent memory corruption, surfaced
  only by a Rust debug-only UB check that a release build would not run. The ABI
  marker now makes it a link error. Bumping `RUNTIME_ABI_VERSION` means renaming
  the marker function in `runtime.rs`; the test in that file fails otherwise.
- **`LoadLocal` clones strings on every read**, and the native backend mirrors
  that with `kira_rt_str_clone` to hold parity. This contradicts the language's
  own model — reading a named non-trivial value should move or borrow, and
  implicit deep copies are what Rust refuses to do — and it heap-allocates per
  string read. It stands only because v0 has no borrow checker. When the checker
  lands, both sides drop the clone in the same change, or parity breaks.
- **`llvm-sys` and the managed LLVM must agree.** `llvm-metadata.toml` is the
  source of truth for the pinned version; a test in `kira-toolchain` fails if the
  `llvm-sys` dependency drifts from it.

## How a hybrid program runs

1. `kirac build|run --backend hybrid` lowers one IR three ways:
   `compile_hybrid` emits the bytecode half (`<stem>.kbc`),
   `build_hybrid_library` emits the native half (`lib<stem>.dylib`) with one
   `kira_native_fn_<id>` trampoline per `@Native` function, and `kira-cli`'s
   `hybrid.rs` writes the `.khm` manifest describing both.
2. `kira-hybrid-runtime` loads the manifest, decodes the `.kbc` into a
   `Program`, proves the two halves agree, `dlopen`s the library, binds each
   trampoline, and installs its invoker.
3. It then runs the entrypoint on whichever engine the manifest records, serving
   as `HostCapabilities` for the VM half and as the invoker target for the
   native half.

The manifest is built in the CLI rather than in `kira-hybrid-definition`:
building one means reading the IR, and the definition crate sits below
`kira-ir` and must not learn about it.

### The engine default has to agree three ways

`compile_hybrid`, the LLVM backend's `build_hybrid`, and the CLI's manifest
builder each resolve `Execution::Inherited` against `Execution::Runtime`. They
must agree function for function — the manifest is what every crossing marshals
against. `kira-hybrid-runtime`'s `validate.rs` is what catches them drifting,
and it runs at load rather than at the first crossing.

## When touching this, check

- `crates/kira-cli/tests/backend_parity.rs` — any lowering change runs here.
- `crates/kira-runtime-abi/src/bridge.rs` — the layout test, if `BridgeValue`
  moves at all.
- `crates/kira-native-bridge/src/runtime.rs` — the marker test, if any
  `kira_rt_*` signature changes.
- `crates/kira-hybrid-runtime/src/marshal.rs` — if the codegen changes what it
  frees at a crossing. The seam's ownership rules are enforced nowhere; that
  file is where they are acted on, and `.codex/work/hybrid-handoff.md` is where
  they are written down.
- Both feature paths: `cargo clippy --workspace --all-targets` with and without
  `--features kira-cli/llvm`. The no-LLVM path is the one CI has.
