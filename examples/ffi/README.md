# Seamless C-FFI example

Ordinary Kira code calls prebuilt C functions with no `@Native` anywhere. A
package lists its native libraries under `NativeLibs/`, declares bodyless
`@FFI.Extern` functions with exact-width signatures, and calls them like any
other function. The same package runs on the VM, on LLVM/native, and on hybrid,
and links for wasm.

See `../../sites/docs/content/docs/appendix/ffi-workflows/` for the full
reference. This directory is the worked example.

## Build the C archive, then run

The archives are not checked in. Build them from `NativeLibs/ffimath.c` with the
managed toolchain, into `NativeLibs/lib/`:

```sh
clang -c NativeLibs/ffimath.c -o NativeLibs/lib/ffimath.o
llvm-ar crs NativeLibs/lib/libffimath.a NativeLibs/lib/ffimath.o

kira run --backend vm main.kira      # 42 / 4 / 0
kira run --backend llvm main.kira
kira run --backend hybrid main.kira
```

For wasm, build the archive with emscripten and target the Web device:

```sh
emcc -c NativeLibs/ffimath.c -o NativeLibs/lib/ffimath-wasm.o
emar crs NativeLibs/lib/libffimath-wasm.a NativeLibs/lib/ffimath-wasm.o

kira build --device wasm32 main.kira
```

## Supported surface

- Parameters and results: `Void`, `Int` and the narrower `I8`/`I16`/`I32` and
  `U8`/`U16`/`U32`/`U64`, `Bool`, `Float` and `F32`, `RawPtr`.
- `CString` **parameters**, which accept a Kira `String` by a transient
  NUL-terminated copy — the caller keeps its `String`, and an interior NUL is a
  typed trap.
- `RawPtr` is an opaque target-width word: Kira stores it, returns it, and passes
  it back, but never dereferences or frees it.
- A narrower C type names its width, because that width is part of the contract.
  `Int` crosses as `int64_t` and `Float` as `double` — they *are* the 64-bit
  types, so nothing is left unsaid by writing them, and there is no `I64` or
  `F64` to write instead. A Kira `String` in a signature is refused.

## Deferred to later milestones

The FFI workflow pages keep the list, and keep it once: this example predates
half of what has landed since, and a second copy of the list is how a reader
learns that structs, arrays, callbacks, and generated bindings are unavailable
when they are not. Everything still deferred is refused with a typed diagnostic
rather than mislowered.
