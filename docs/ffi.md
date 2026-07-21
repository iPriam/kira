# Seamless C-FFI

Kira calls prebuilt C functions with no `@Native` and no glue: a package lists
its native libraries, declares bodyless `@FFI.Extern` functions, and calls them
like any other Kira function. The same package runs identically on the VM, on
LLVM/native, and on both halves of hybrid, and links for wasm.

A worked example lives in [`examples/ffi`](../examples/ffi).

## Declaring an extern

An `@FFI.Extern` declaration names the native library, the C symbol, and the C
ABI, and its function has an exact-width signature and no body:

```kira
@FFI.Extern { library: ffimath; symbol: ffi_add; abi: c; }
function add(a: I32, b: I32) -> I32;

@Main function main() { print(add(20, 22)) return }
```

The three fields are all required, exactly once each; `abi` must be `c`. A
foreign declaration may not also be `@Main`, `@Runtime`, `@Native`, or
`@Export`. Each rule is a stable diagnostic, not a silent acceptance.

## The supported type surface

Parameters and results may be `Void`, the fixed-width integers `I8`/`I16`/`I32`/
`I64` and `U8`/`U16`/`U32`/`U64`, `Bool`, `F32`/`F64`, and `RawPtr`. Fixed-width
integer names are mandatory because the C width is part of the contract — bare
`Int` and `Float` are refused, as is a Kira `String` in a signature.

`CString` is a parameter-only type. A call passes a Kira `String` where a
`CString` parameter is expected — the one implicit coercion — and the value is
copied into transient NUL-terminated storage for the duration of the call; the
caller keeps its `String`. An interior NUL byte is a typed trap rather than a
truncated string. `CString` is illegal as a local, a field, an ordinary
parameter or result, or an extern **result**: returned-string ownership is
unspecified and deferred.

`RawPtr` is an opaque target-width word. Kira may store it, return it, and pass
it back to C, but never dereferences it, does arithmetic on it, or frees it — a
library that allocates must expose an explicit free extern. A null pointer is
just data and round-trips like any other word.

## How one declaration serves every backend

Every backend reaches C through one generated **adapter** per import — a uniform
`extern "C"` entry point that validates the argument tags, narrows and
sign/zero-extends at the C boundary, rounds `F32`, builds and frees transient
`CString` storage, calls the real C symbol, and writes back a checked result.

- **LLVM/native** links the selected C archive into the executable and calls the
  adapter directly.
- **The VM** stays a portable core: it marshals a foreign call to borrowed
  arguments and asks its host, loading and linking nothing itself. A VM build
  emits a one-file adapter *sidecar* — the adapters plus the C archive — and the
  CLI's native-capable host answers `call_foreign` out of it.
- **Hybrid** links the C archive and the adapters into its one native half.
  A `@Runtime` function's foreign call runs through the VM half's `call_foreign`
  and a `@Native` function's runs as machine code, and both reach the *same* copy
  of the C library — never a second `dlopen`. The example's counter proves it:
  a runtime-half call and a native-half call count 1 then 2, not 1 then 1.

## Native libraries

A package declares a native library with a `NativeLibs/<name>.toml` manifest that
names the library and one `[[target]]` row per target it ships an archive for:

```toml
name = "ffimath"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffimath.a"
[[target]]
triple = "wasm32-emscripten-unknown"
staticLib = "lib/libffimath-wasm.a"
```

Paths resolve relative to the manifest. Target selection is exact and
structural: a host build picks the row whose triple matches this machine, and
`--device wasm32` picks the `wasm32-emscripten-unknown` row. A library declared
only for the host and asked for wasm is refused before `emcc` runs, with a
diagnostic naming the library and the target.

## Wasm

A wasm build passes the matching `wasm32-emscripten` archive and the generated
adapters to `emcc` and runs under a JS host. Every supported scalar and `RawPtr`
type works; the example's scalar program links its emscripten archive and runs
under node.

`CString` on wasm is not yet usable, and the reason is not FFI: Kira string
creation on `wasm32` is blocked by a width mismatch in the `kira_rt_str_new`
runtime helper (its length is emitted as 64-bit but the wasm runtime archive
expects a 32-bit `usize`), which affects any wasm program that builds a string at
all. `CString` length is proven on the host backends, where string creation is
sound, and wasm `CString` follows once that helper's width is fixed.

## Deferred to later milestones

`CString` results, aggregates (structs, arrays, enums) across the seam, callbacks
and function pointers, non-C ABIs, variadics, generic externs, header parsing and
autobind, dynamic-only C libraries, and compiling native-library sources. Each is
refused today with a typed diagnostic rather than mislowered.
