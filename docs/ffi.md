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

A package declares a native library in `package.kira`, or in a
`NativeLibs/<name>.toml` beside it. Both spellings mean the same thing, and a
package may use either or both — the only difference is where relative paths are
anchored: the package root for an inline entry, the file's own directory for a
TOML.

```text
let nativeLibraries = [
    NativeLibrary {
        name: "sokol",
        linkMode: LinkMode.Static,
        headers: Headers { entrypoint: "NativeLibs/Sokol/sokol.h", defines: ["SOKOL_NO_ENTRY"] },
        sources: ["NativeLibs/Sokol/sokol_impl.c"],
        nativeTargets: [
            NativeTarget { triple: "aarch64-macos-none", staticLib: "generated/libsokol.a",
                           frameworks: ["AppKit", "QuartzCore"] },
            NativeTarget { triple: "x86_64-linux-gnu", staticLib: "generated/libsokol.a",
                           systemLibs: ["X11", "GL"] }
        ],
    }
]
```

```toml
[library]
name = "ffimath"
link_mode = "static"

[target.aarch64-macos-none]
static_lib = "lib/libffimath.a"

[target.wasm32-emscripten-unknown]
static_lib = "lib/libffimath-wasm.a"
compiler_flags = ["--use-port=emdawnwebgpu"]
```

Target selection is exact and structural: a host build picks the row whose
triple matches this machine, and `--device wasm32` picks the
`wasm32-emscripten-unknown` row. A library declared only for the host and asked
for wasm is refused before `emcc` runs, with a diagnostic naming the library and
the target.

A selected row contributes more than an archive. Its `frameworks`, `systemLibs`,
and `linkerFlags` go on the same link line, so a library whose symbols come from
Apple frameworks needs no archive at all — write the row with neither
`staticLib` nor `dynamicLib`. Under `LinkMode.Dynamic`, a row that names nothing
whatsoever links the library by its own name (`dynamicLib: ""` on a library
called `vulkan` is `-lvulkan`); the same row under `LinkMode.Static` is refused,
because it says nothing about what to link.

`headers`, `sources`, and `autobind` are read and carried, and not yet acted on:
a library's own C sources are not compiled for it, and bindings are not
generated. Ship the archive.

## Structs by value

A `@FFI.Struct { layout: c }` crosses the seam **by value**, as a parameter or a
result, when every field is a fixed-width scalar, `Bool`, `RawPtr`, or another
such struct — to any depth:

```text
@FFI.Struct { layout: c; }
struct Rect { var x: F64
var y: F64 }

@FFI.Extern { library: graphics; symbol: rect_scale; abi: c; }
function rectScale(r: Rect, k: F64) -> Rect;
```

The annotation is required. An ordinary Kira struct is refused even when its
fields would all map, because the annotation is what says this type mirrors a C
declaration field for field — without it, adding a Kira field would silently
change what the C function receives.

**Kira never classifies the ABI.** Passing a struct by value is the one place
the C ABI cannot be derived from the type alone: x86-64 System V classifies
eightbytes, AArch64 AAPCS detects homogeneous float aggregates and returns large
ones indirectly, and wasm32 has its own rules. So for each import naming a
struct, `kirac` generates a small C file that redeclares the struct, redeclares
the real symbol with its true by-value signature, and wraps the call in a shim
taking every aggregate through a pointer. The target's own C compiler builds it
— the managed clang for a host build, `emcc` for wasm — and applies the ABI it
defines. Everything Kira emits speaks only pointers and scalars.

A field the seam cannot carry is refused by name. A `@FFI.Callback` member, a
`CString`, and any Kira heap type still are.

### Inline arrays

A C struct that reserves storage inline — `int cells[4]` — is spelled with an
`@FFI.Array` typedef, and the struct names that type as a field:

```text
@FFI.Array { element: I32; count: 4; }
struct Cells4 {}

@FFI.Struct { layout: c; }
struct Grid { var cells: Cells4
var weight: F64 }
```

The elements live in one Kira field named `elements`, so `grid.cells.elements[2]`
is ordinary array indexing and `Cells4 { elements: [1, 2] }` is an ordinary
struct literal. Indexing the typedef itself (`grid.cells[2]`) names that field
instead of guessing.

Kira's array length and the C extent are different things, so the seam fixes
what happens when they differ. Fewer elements than the extent fill from the
front and leave the rest **zero** — the same value a zero-filled construction
carries, and the same bytes on every backend. More elements than the extent is a
trap, on the VM and in native code alike: the elements past the extent have
nowhere to go, and writing only the ones that fit would hand C a value the
program did not write. A result always carries the whole extent back, because C
storage has no length of its own.

An `@FFI.Array` type crosses as a **member**. In a parameter or result position
C decays an array to a pointer — a different type with different ownership — so
the seam refuses it there and asks for `RawPtr` when that is what the symbol
takes.

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

## Binding type vocabulary (`bind-types/`)

A generated binding leans on C primitive typedefs it never defines — `VkFlags`,
`UINT`, `BOOL`, `HRESULT`. Until header-driven autobind emits them, define each
as a transparent alias to its Kira scalar (`type VkFlags = U32`,
`type HANDLE = RawPtr`) in a `*_types.kira` file. The alias resolves away in the
frontend, so a use site and its scalar are the same type on every backend.

Such a file must live in a `bind-types/` directory — a peer of `bindings/`,
kept apart from a package's own `types/` domain types and from generated
`bindings/`. A `*_types.kira` source anywhere else is rejected with `KPK025`.
The rule is a convention enforced at project discovery, not a compiler
mechanism; delete the file once autobind emits the typedefs into the binding.

## Deferred to later milestones

`CString` results, Kira enums and heap types across the seam, callbacks and
function pointers, non-C ABIs, variadics, generic externs, header parsing and
autobind, dynamic-only C libraries, and compiling native-library sources. Each is
refused today with a typed diagnostic rather than mislowered.
