# Seamless C-FFI

Kira calls prebuilt C functions with no `@Native` and no glue: a package lists
its native libraries, declares bodyless `@FFI.Extern` functions, and calls them
like any other Kira function. The same package runs identically on the VM, on
LLVM/native, and on both halves of hybrid, and links for wasm.

A worked example lives in [`examples/ffi`](../examples/ffi). The `@FFI.Extern`
form's sibling, `@FFI.Syscall`, calls the Linux kernel with no library at all —
see [Calling the kernel](#calling-the-kernel-ffisyscall).

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

Parameters and results may be `Void`, `Int` and the narrower integers
`I8`/`I16`/`I32` and `U8`/`U16`/`U32`/`U64`, `Bool`, `Float` and `F32`, and
`RawPtr`. `Int` crosses as `int64_t` and `Float` as `double` — they *are* the
64-bit types, so nothing is left unsaid by writing them; a narrower C type still
names its width. A Kira `String` in a signature is refused.

`CString` is C text, and how long its storage lives depends on where it is
written. As a **parameter**, a call passes a Kira `String` — the one implicit
coercion — and the bytes are copied into transient NUL-terminated storage for
the duration of the call; the caller keeps its `String`. An interior NUL byte is
a typed trap rather than a truncated string. `CString` is illegal in an
ordinary Kira local or function parameter. An extern result may be `CString`:
the seam copies the returned `const char*` into an owned Kira `String` while the
pointer is valid, so no C storage crosses the result boundary.

As a **member of a C-layout struct** it is a pointer word, and its storage is
never released because C may retain the descriptor after the call and the seam
cannot know when it is safe to free it. This leaks one allocation per distinct
string; building one per frame grows memory. A member left out of the literal
zero-fills to `NULL`, which is different from a pointer to `""`.

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
        linkMode: .Static,
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
`staticLib` nor `dynamicLib`. Under `.Dynamic`, a row that names nothing
whatsoever links the library by its own name (`dynamicLib: ""` on a library
called `vulkan` is `-lvulkan`); the same row under `.Static` is refused,
because it says nothing about what to link.

A row may also name `runtimeFiles`: what the finished program has to find beside
itself when it *starts*, as opposed to what the linker reads.

```text
NativeTarget {
    triple: "x86_64-windows-msvc",
    dynamicLib: "NativeLibs/Dawn/x86_64-windows-msvc/lib/webgpu_dawn.lib",
    runtimeFiles: ["NativeLibs/Dawn/x86_64-windows-msvc/bin"]
}
```

A dynamic library is two files, and naming only the first leaves a program that
links clean and cannot start. Each entry is copied beside every link output — a
file as itself, a directory as its files one level deep — because the loader
searches the program's own directory before anything else. A directory rather
than three named DLLs is what a vendor's release payload actually is, and Dawn
opens one of them (`dxcompiler.dll`) by name at run time with no link-time
mention of it at all. An entry missing on the target being built is refused by
name, since the alternative is a loader status code naming no file.

`headers` and `sources` describe how the library is built, and `autobind`
describes what to call in it — see [Generated bindings](#generated-bindings)
below.

## Structs by value

A `@FFI.Struct { layout: c }` crosses the seam **by value**, as a parameter or a
result, when every field is a fixed-width scalar, `Bool`, `RawPtr`, or another
such struct — to any depth:

```text
@FFI.Struct { layout: c; }
struct Rect { var x: Float
var y: Float }

@FFI.Extern { library: graphics; symbol: rect_scale; abi: c; }
function rectScale(r: Rect, k: Float) -> Rect;
```

The annotation is required. An ordinary Kira struct is refused even when its
fields would all map, because the annotation is what says this type mirrors a C
declaration field for field — without it, adding a Kira field would silently
change what the C function receives.

**Kira never classifies the ABI.** Passing a struct by value is the one place
the C ABI cannot be derived from the type alone: x86-64 System V classifies
eightbytes, AArch64 AAPCS detects homogeneous float aggregates and returns large
ones indirectly, and wasm32 has its own rules. So for each import naming a
struct, `kira` generates a small C file that redeclares the struct, redeclares
the real symbol with its true by-value signature, and wraps the call in a shim
taking every aggregate through a pointer. The target's own C compiler builds it
— the managed clang for a host build, `emcc` for wasm — and applies the ABI it
defines. Everything Kira emits speaks only pointers and scalars.

A field the seam cannot carry is refused by name: an ordinary Kira heap value
such as `String`, an array, or an enum with a payload. `CString`,
`@FFI.Array`, and `@FFI.Callback` fields cross in their supported positions;
see above for their storage rules.

### A struct passed by address

A parameter written as an `@FFI.Pointer` to a C-layout struct also accepts that
struct itself, which is what `sapp_run(move desc)` means: the seam writes the
struct's C-layout image and passes its address. The image gets the same storage
a `CString` member does, and for the same reason — the callee may keep the
pointer, and nothing on this side knows whether it did.

The same position accepts an **`@FFI.Array` of that struct**, which is the same
image with an extent: a C array is its elements laid out end to end, so one row
describes both and the address of the row is the address of element zero.

```text
@FFI.Array { element: WGPUVertexAttribute; count: 4; }
struct WGPUVertexAttributeArray4 {}

WGPUVertexBufferLayout {
    arrayStride: 20
    attributeCount: 2
    attributes: WGPUVertexAttributeArray4 { elements: [position, colour] }
}
```

That is what makes a descriptor-driven graphics API reachable. Almost none of
them take one item; they take `T const *items` beside an `itemCount` — vertex
attributes, bind group entries, colour targets — and without a way to name
several items' storage from Kira, that descriptor could only be built in C. A
Kira array shorter than the extent zero-fills the rest, exactly as it does for
an inline-array member, and the count travelling beside the pointer is what says
how many C reads.

A third value fills the same position: a struct that **begins** with the target.
A struct and its first member share an address, so writing the extension writes
the base too:

```text
wgpuInstanceCreateSurface(instance, WGPUSurfaceDescriptor {
    nextInChain: WGPUSurfaceSourceWindowsHWND {
        chain: WGPUChainedStruct { sType: 5 }
        hinstance: instance
        hwnd: window
    }
    label: title
})
```

`nextInChain` is declared `WGPUChainedStruct *`; the value is the HWND source,
whose first member is that chain. This is how every extensible C header extends
— WebGPU's `nextInChain`, Vulkan's `pNext` — and C walks the chain by casting a
link back to whichever extension it belongs to. Only the *first* member counts:
anything later sits at a nonzero offset and is a different address. Without it,
one extension costs a redeclared descriptor carrying that extension's own
pointer type, and the mirror stops matching the header the moment it changes.

All three fills work at a **member** as well as at an argument, and a member
written as an `@FFI.Pointer` is a pointer word exactly as a `RawPtr` member is —
so the array-of-scalars fill below reaches a typed pointer member too, not only a
bare `RawPtr` one.

### Reading members through a pointer

A pointer whose `target` is a C-layout struct keeps that target, so the members
behind it are read directly:

```text
@FFI.Struct { layout: c; }
struct sapp_event {
    let kind: U8 = 0
    let mouse_x: F32 = 0.0
}

@FFI.Pointer { target: sapp_event; ownership: borrowed; }
struct sapp_event_ptr {}

function onEvent(event: sapp_event_ptr) {
    if event.kind == 3 { moveTo(event.mouse_x) }
}
```

This is what a callback argument needs. C hands over `const sapp_event*` and the
members behind it are the whole payload; without the read, each one needs an
accessor compiled into a shim — twenty of them, for sokol's event struct alone.

A read lowers to a load at the member's offset in the target's C layout. The
offset is computed per target, because a C pointer is four bytes on `wasm32` and
eight elsewhere, so a struct with a pointer member ahead of the one being read
lays out differently.

A member is one of two things. A scalar reads back as a value — that is the
load. A nested struct or an inline array has its bytes *inside* the container, so
it names a place, and reading it gives that place's address:

```text
let x = event.at.x                    // nested struct
let y = event.touches[index].pos_y    // inline array, indexed like C's
```

An array member decays to a pointer to its first element and indexing walks from
there, both with the meaning C gives them. Nothing is copied out of C storage:
every step is an address until the last, which is the load.

A pointer whose target is not a declared C-layout struct stays an opaque handle
and reads nothing, which is deliberate: generated bindings point at C types
nobody declared and at themselves, and neither is a mistake.

An `@FFI.Pointer` member left out of a literal is `NULL`, on the same terms as a
`RawPtr` one — a typed pointer is the same word, and knowing what it addresses
cannot change what zeroing it means. That is what makes a modern graphics
descriptor writable at all: WebGPU gives every one of them a
`WGPUChainedStruct *nextInChain` plus an optional `T const *` per feature, and

```text
WGPURenderPassColorAttachment { view: view, loadOp: 2, storeOp: 1 }
```

leaves the chain, the resolve target and the rest at `NULL` without naming them.
`RawPtr` and an `@FFI.Pointer` also pass for each other at an argument and at a
member, so one address may be held as `RawPtr` and handed to a symbol that
declares the typed pointer.

### Inline arrays

A C struct that reserves storage inline — `int cells[4]` — is spelled with an
`@FFI.Array` typedef, and the struct names that type as a field:

```text
@FFI.Array { element: I32; count: 4; }
struct Cells4 {}

@FFI.Struct { layout: c; }
struct Grid { var cells: Cells4
var weight: Float }
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

An `@FFI.Array` type crosses as a **member**, and — as
[A struct passed by address](#a-struct-passed-by-address) describes — as the
storage a pointer to its element type addresses. What it is not is a value C
receives by itself: in a parameter or result position C decays an array to a
pointer, a different type with different ownership, so the seam refuses it there
and asks for `RawPtr` when that is what the symbol takes.

## Arrays as C buffers

A Kira array fills a **pointer word** — an extern's `RawPtr` parameter, or a
C-layout struct's `RawPtr` member — as long as its elements are seam scalars.
The seam writes the elements out at C's widths and the pointer is the address of
what it wrote:

```text
@FFI.Extern { library: ffimath; symbol: sum_floats; abi: c; }
function sumFloats(values: RawPtr, count: I32) -> F32;

let values: [F32] = [1.5, 2.25, 3.0]
sumFloats(values, I32(values.count))            // pointer and a count

@FFI.Struct { layout: c; }
struct Range { var ptr: RawPtr
var size: U64 }

Range { ptr: values, size: U64(values.count * 4) }   // the same buffer, named
```

Writing the elements out is the whole point, not a copy that could be skipped:
Kira holds a `[F32]` as `double`s and C reads four bytes each, so handing over
the array's own storage would give C wrong *numbers* rather than a wrong
pointer — a rendering bug rather than a crash. An empty array is a null pointer.

The buffer gets the storage a `CString` member gets, and is never reclaimed, for
the same reason: `sg_make_buffer` reads it during the call but `sg_range` handed
to `sg_apply_uniforms` may be kept, and nothing this side knows which kind of
callee it has.

The member position is what makes a graphics API reachable at all. Almost none
of them take a pointer and a count as two arguments; they take a descriptor
holding both, and without a way to name an array's address inside a struct
literal that descriptor can only be built in a C helper.

## Callbacks

A `@FFI.Callback` declares a C function-pointer type. Its value can be a C
function pointer or a named top-level Kira function. It crosses both as a
struct member and on its own:

```text
@FFI.Callback { abi: c; params: [I32, I32]; result: I32; }
struct Adder {}

@FFI.Struct { layout: c; }
struct Hooks { var add: Adder
var scale: I32 }

@FFI.Extern { library: demo; symbol: run_hooks; abi: c; }
function runHooks(h: Hooks, a: I32, b: I32) -> I32;
```

A pointer C hands out can be stored and passed back. A named top-level Kira
function fills one: `Hooks { add: combine, scale: 2 }`. C then calls into Kira
through the generated entry:

- The value is the address of a generated entry thunk, one per (function,
  signature) pair, named `kira_ffi_callback_<i>`.
- On a native build the thunk calls the compiled function directly. Under the
  VM it reaches the interpreter through the adapter sidecar, on the same door
  the hybrid native half uses to call a `@Runtime` function. C cannot tell the
  two apart, which is the point.
- The function's declared types must match the callback's, position for
  position, under the same exact-width rule the extern seam applies: a bare
  `Int` is not a callback parameter any more than it is an extern one.
- A bare top-level function name is accepted only where a C callback is
  expected. Function-typed closure values are a separate
  `@Native`/`@Runtime` bridge feature, and a local of the same name wins.

Zero-fill gives a callback member `NULL`, so `Hooks {}` is the no-callback case
and C sees it as null.

A callback signature may carry fixed-width scalars, `Bool`, `RawPtr`,
`CString`, and a `@FFI.Struct { layout: c }` that C passes by value. Its result
may be a scalar or nothing. Aggregate callback results remain unsupported. A
generated binding may declare a callback shape the seam cannot carry, or has
not seen defined; handing a Kira function to that shape is refused with
`KSEM245`.

The closure bridge currently covers scalar `let` captures. It does not make a
closure or method a `@FFI.Callback` value, and aggregate captures remain
outside this path.

### A struct C passes by value

A C callback may receive a C-layout struct by value. Kira receives it **as a
pointer to the struct**, the way a `const sapp_event*` argument does:

```text
@FFI.Struct { layout: c; }
struct FfiView { let data: CString
let length: U64 }

@FFI.Pointer { target: FfiView; ownership: borrowed; }
struct FfiViewPtr {}

@FFI.Callback { abi: c; params: [I32, FfiView]; result: Int; }
struct Viewer {}

function onView(tag: I32, view: FfiViewPtr) -> Int { return view.length }
```

Kira never classifies the by-value parameter, exactly as it never classifies a
by-value argument. The address C holds is a generated C entry that takes the
struct by value — so the target's own C compiler decides how it arrives — and
calls the thunk with `&param`. The pointer is good for the call, which is the
lifetime a callback argument has, so members are read through it rather than
kept.

A callback **result** stays scalar. A parameter is storage C already owns, but a
result would require the seam to build and return C-layout bytes.

## Wasm

A wasm build passes the matching `wasm32-emscripten` archive and the generated
adapters to `emcc` and runs under a JS host. Every supported scalar and `RawPtr`
type works; the example's scalar program links its emscripten archive and runs
under node.

`CString` works on wasm in both directions — a Kira string copied into transient
C storage for a call, and a returned `const char*` copied back out into an owned
Kira `String` — and so do Kira strings on their own. Both are proven under node
by `a_wasm_build_creates_kira_strings_and_crosses_the_cstring_seam`.

They were not, until the length passed to `kira_rt_str_new` stopped being
emitted at the *host's* pointer width. The emscripten archive expects a 32-bit
`usize`; the 64-bit call resolved by name at link time and every wasm string
trapped. The backend now declares that parameter at the target's width, which is
also why `Types` carries a `usize_ty` rather than reusing `i64`.

## Generated bindings

A library that declares `autobind` gets its `@FFI.Extern` declarations written
for it, from its own headers, before anything is analyzed:

```text
autobind: Autobind { module: "text", headers: ["NativeLibs/Text/kira_text.h"], mode: AutobindMode.AllPublic },
```

`kira check`, `run`, `build`, `test`, and `live` all generate first, for every
package in the dependency graph — a library is declared by the package that owns
it, so an app importing UI Foundation gets `kiratext` bound into UI Foundation's
own `app/bindings/text.kira`. The generated file is ordinary Kira source in the
dialect above, compiled with the rest of that package, and readable: nothing
about it is generator-private.

The C parser is the managed toolchain's own `libclang`, and every width is read
from it rather than assumed. `long length` binds as `Int` on macOS and `I32`
under MSVC because that is what `long` *is* on each — the mistake a hand-written
binding makes silently, and the reason this is the compiler's job.

**What is bound.** Functions come from the listed headers and nowhere else: a
header includes `<stdio.h>` to get a `FILE *`, and binding what it reaches would
declare libc into every package that used it, colliding on `fread` with the next
library that did the same. `AllPublic` binds every function those headers
declare; `Selected` binds the ones the declaration names. Types follow the
signatures, at any depth, plus the structs the headers define.

**What is not, and why you can tell.** A variadic function has no fixed
signature, a `long double` has no portable width, `char *argv[]` has no length.
Each is written into the generated file as a comment naming the declaration and
the reason, so a missing function reads as a decision rather than a gap. A
hand-written `@FFI.Extern` for one of them would be refused by the same rule.

**Caching.** A binding is regenerated when it is missing or when a header it was
generated from has changed; a stamp under `.kira-build/autobind/` records what
it was generated from, and an unchanged tree loads no C parser at all. A binding
that exists with *no* stamp is the package's own source — `kira-graphics` ships
its Vulkan and Direct3D bindings because regenerating them needs an SDK that is
not on every machine — so it is adopted as it stands and reported once (KPK041).
Delete it to have it generated.

## Binding type vocabulary (`bind-types/`)

A *hand-written* binding leans on C primitive typedefs it never defines —
`VkFlags`, `UINT`, `BOOL`, `HRESULT`. Define each as a transparent alias to its
Kira scalar (`type VkFlags = U32`, `type HANDLE = RawPtr`) in a `*_types.kira`
file. The alias resolves away in the frontend, so a use site and its scalar are
the same type on every backend.

Such a file must live in a `bind-types/` directory — a peer of `bindings/`,
kept apart from a package's own `types/` domain types and from generated
`bindings/`. A `*_types.kira` source anywhere else is rejected with `KPK025`.
The rule is a convention enforced at project discovery, not a compiler
mechanism.

A generated binding needs none of this: autobind resolves each typedef through
clang and writes the scalar the target actually uses, so `VkFlags` arrives as
`U32` at every use with nothing to declare.

## Calling the kernel: `@FFI.Syscall`

`@FFI.Extern`'s sibling, and the one member of the family that names no library:

```kira
@FFI.Syscall { name: write; }
function sysWrite(fd: Int, buffer: CString, count: U64) -> Int;

@FFI.Syscall { name: exit_group; }
function sysExit(status: Int);
```

It lowers to one instruction — `svc #0` on AArch64, `syscall` on x86-64 — with
the call number and the arguments in the registers the kernel reads them out of.
Nothing is bound, nothing is looked up, and no libc is involved, which is the
whole point: the program this exists for is PID 1 in an initramfs with no dynamic
loader, and a `write` that resolves through a shared object is a program that
never starts. `packages/linux` is the typed surface over it, and
`tests-kik/syscall-harness` is the suite.

**The program names the call and the compiler owns the number.** That is not a
convenience. `write` is 64 on AArch64 and 1 on x86-64, and Kira has no
conditional compilation and no way for a program to ask what machine it is being
built for — so a number written in Kira source would have to be wrong on one of
them. Naming the call keeps the source arch-neutral and lets an unsupported call
or an unsupported machine be refused *by name*, at compile time, instead of
entering the kernel with a number that means something else there.

The block carries one field, `name`, written the way `man 2` writes it —
`exit_group`, not `exitGroup` — so a declaration is something a reader can look
up. `library`, `symbol`, and `abi` are refused rather than ignored: an author who
wrote `library: libc` believes the call goes through libc, and it does not. The
calls the compiler has numbers for are `read`, `write`, `mount`, `umount2`,
`reboot`, `execve`, `wait4`, `exit_group`, and `sync`; any other name is
`KSEM279`, which lists them.

The signature is register words, because that is what the kernel entry carries:
at most six arguments (`KSEM282` for a seventh — there is no register for it), and
each of them an integer of any width, a `RawPtr`, or — as an argument only — a
`CString`, whose transient NUL-terminated copy is what the pointer register
receives. A `Float` has no register in the sequence and an aggregate has no single
word, so both are `KSEM283`. A declaration's written width is what reaches the
register: `fd: I32` arrives sign-extended exactly as it would through an
`@FFI.Extern`, so one annotation never disagrees with the other about the same
signature.

`exit_group` does not return, and the compiler knows: a declaration that names a
result for it is `KSEM284`, because that is a value nothing can ever produce, and
the code generator marks the point after the call unreachable so a caller writes
no statement it could not reach.

Two refusals are about the machine, and they are separate because they have
separate fixes. `KSEM280` is a target that is not Linux — macOS is deliberately
excluded even on the same processors, because its numbers are not a stable
interface: Apple supports libSystem and the numbers move between releases, so
Kira has none it would stand behind. `KSEM281` is Linux on a processor Kira emits
no entry sequence for; it emits for `aarch64` and `x86_64`.

Both are asked of the machine the build *targets*, not the one running the
compiler, so `kira check --target aarch64-linux-gnu` from a Windows host answers
about the aarch64 program rather than about Windows.

The VM and hybrid engines refuse to start a program that calls the kernel, by
name, before it runs. A system call is an instruction and the interpreter has
none of its own to put one in — what a bytecode module carries has to load on
every machine Kira runs on, including a `wasm32` one where there is no entry
sequence at all — so reaching the kernel from it would mean the *host's* numbers
on the *host's* architecture, which is a different call from the one the program
named. `--backend llvm` is what emits it.

## Deferred to later milestones

Payload-carrying Kira enums as direct C values, direct `@FFI.Array` values in
parameter or result positions, aggregate callback results, non-C ABIs,
variadics, generic externs, and dynamic-only C libraries. Payload-less enums
cross as C enum integers. `@FFI.Array` values cross as C-layout members or
pointer-backed storage, and `@FFI.Callback` values cross as callback members or
parameters. Each deferred shape is refused with a typed diagnostic rather than
mislowered.
