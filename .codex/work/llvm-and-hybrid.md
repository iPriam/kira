# LLVM/native and hybrid

Orientation for the two non-VM backends. The VM path (`kira-bytecode` →
`kira-vm-runtime`) is not covered here.

## Status

- **LLVM/native: landed.** `kirac build|run --backend llvm` produces a real
  native executable. Parity with the VM is proven, not asserted: eleven
  differential tests plus every example, identical stdout and exit status
  (`crates/kira-cli/tests/backend_parity.rs`).
- **Hybrid: half-built.** Both ends of the boundary exist and are tested; the
  middle does not. See "What is left" below.

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

Each is append-only and has a test that pins it. Kai changes one only together
with the code that emits or reads it.

| Contract | Defined in | Shape |
|---|---|---|
| `kira_rt_*` | `kira-native-bridge/src/runtime.rs` | native runtime: print, strings, div-zero trap |
| ABI marker | `kira-runtime-abi/src/lib.rs` | `RUNTIME_ABI_VERSION` / `RUNTIME_ABI_MARKER` |
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

## What is left for hybrid

Both ends exist: the frontend carries `@Runtime`/`@Native` to the IR,
`compile_hybrid` splits the bytecode half, the codegen emits the native half
with trampolines, and both call directions are built. Missing:

1. **`kira-hybrid-runtime`** — still the stub. It is the host: load the manifest
   and bytecode, `dlopen` the library (`libloading`), bind each trampoline,
   install the invoker, and implement `HostCapabilities` so `call_native`
   marshals `NativeArg`/`NativeResult` to and from `BridgeValue`.
2. **CLI wiring** — `build|run --backend hybrid` writes `.khm` + `.kbc` +
   library, then runs the program in the host. `pipeline.rs` reports hybrid as
   unimplemented today.
3. **Parity tests** — a hybrid program must match the VM and native builds of the
   same source.

The contracts above are fixed, so this is codegen-and-loader work, not design.

## When touching this, check

- `crates/kira-cli/tests/backend_parity.rs` — any lowering change runs here.
- `crates/kira-runtime-abi/src/bridge.rs` — the layout test, if `BridgeValue`
  moves at all.
- `crates/kira-native-bridge/src/runtime.rs` — the marker test, if any
  `kira_rt_*` signature changes.
- Both feature paths: `cargo clippy --workspace --all-targets` with and without
  `--features kira-cli/llvm`. The no-LLVM path is the one CI has.
