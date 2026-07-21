# Seamless C-FFI example

Ordinary Kira code calls prebuilt C functions with no `@Native` anywhere. A
package lists its native libraries under `NativeLibs/`, declares bodyless
`@FFI.Extern` functions with exact-width signatures, and calls them like any
other function. The same package runs on the VM, on LLVM/native, and on hybrid,
and links for wasm.

See `../../docs/ffi.md` for the full reference. This directory is the worked
example.

## Build the C archive, then run

The archives are not checked in. Build them from `NativeLibs/ffimath.c` with the
managed toolchain, into `NativeLibs/lib/`:

```sh
clang -c NativeLibs/ffimath.c -o NativeLibs/lib/ffimath.o
llvm-ar crs NativeLibs/lib/libffimath.a NativeLibs/lib/ffimath.o

kirac run --backend vm main.kira      # 42 / 4 / 0
kirac run --backend llvm main.kira
kirac run --backend hybrid main.kira
```

For wasm, build the archive with emscripten and target the Web device:

```sh
emcc -c NativeLibs/ffimath.c -o NativeLibs/lib/ffimath-wasm.o
emar crs NativeLibs/lib/libffimath-wasm.a NativeLibs/lib/ffimath-wasm.o

kirac build --device wasm32 main.kira
```

## Supported surface

- Parameters and results: `Void`, `I8`/`I16`/`I32`/`I64`, `U8`/`U16`/`U32`/`U64`,
  `Bool`, `F32`/`F64`, `RawPtr`.
- `CString` **parameters**, which accept a Kira `String` by a transient
  NUL-terminated copy — the caller keeps its `String`, and an interior NUL is a
  typed trap.
- `RawPtr` is an opaque target-width word: Kira stores it, returns it, and passes
  it back, but never dereferences or frees it.
- Fixed-width integer names are mandatory, because the C width is part of the
  contract. Bare `Int`/`Float` and a Kira `String` in a signature are refused.

## Deferred to later milestones

`CString` **results** (returned-string ownership is unspecified), aggregates
(structs, arrays, enums) across the seam, callbacks and function pointers,
non-C ABIs, variadics, generic externs, header parsing / autobind, dynamic-only
C libraries, and compiling native library sources. Each is refused today with a
typed diagnostic rather than mislowered.
