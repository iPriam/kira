# Kira 1.7.5

Kira's compiler and runtime are now a Rust implementation, written from
scratch. The Zig implementation it replaces is 130,372 lines; this one is
141,604 lines of compiler and runtime source, plus 31,611 lines of Rust tests
and 24,162 lines of Kira. It was designed, built, and brought to parity in
fifteen days, and 51,683 of those source lines were written in the final six.

Nothing here is a transliteration. The old implementation was used as a
behavior oracle and nothing else: it answered "what should this program do?",
and every format, layout, and ABI in this release was designed fresh and proven
against it by differential runs rather than by inheritance.

## Four backends, one language

Every language feature in this release runs on all four backends — the
bytecode VM (`kira run`), LLVM/native (`kira build`), WebAssembly, and the
hybrid `@Runtime`/`@Native` split that lets one program straddle both. Parity
is not a roadmap item that trails the VM; a feature that runs in one place runs
in all of them, and the backend parity suite proves it by running the same
program on each and comparing stdout and exit status.

The language that arrived on those backends: structs with methods and value
semantics, enums including generic ones and struct payloads, classes,
closures, arrays, `match` and `switch`, `for` with `break`/`continue`, the
bitwise ladder and `? :`, fixed-width scalar types with C's mixed-width rules,
type aliases, `attempt`/`try`/`handle`, and explicit ownership at the call
site with `borrow` and `borrow mut` carried through function types, bindings,
and function values.

Around it: a multi-file frontend with file-scoped imports and dependency-order
module loading, packages with their own name tables and a visibility gate,
labeled arguments, parameter defaults, and constructs with child slots.

## A foreign function interface with no escape hatch

Kira calls C without an annotation marking the seam. C-layout structs cross by
value, as do inline C array members, C function pointers, and Kira functions
passed to C as callbacks. Native libraries are declared in the manifest,
resolved across the dependency closure, and allowed to be absent on a platform
that does not have them. The VM reaches the same C the native backend does,
routed through Kira's own host rather than through a second mechanism.

## Shaders, as a userland macro

KSL is a shader language with its own parser, resolver, type checker, and IR.
A checked shader lowers to bindings, layouts, and KSLR1 reflection, and emits
Metal, WebGPU, and OpenGL. It is reached through a userland Kira macro; the
compiler builtin that bootstrapped it has been deleted.

Project Matter, the editor built on Kira, renders on Metal.

## Tooling

`knvm` manages toolchains: `knvm install latest` provisions a compiler, its
language server, and the Foundation library, and the release job refuses to
publish an archive it has not installed and run.

The language server ships inside the toolchain archive so the two cannot
drift, and serves diagnostics. Kira Live hot-reloads a running app across the
`.klbundle` boundary. A Kira library can be exported as a Rust crate a Rust
program links and calls, against a native or hybrid engine.

## Runtime

The runtime stopped copying things it can share. Arrays share their storage
and their elements until a write forces the split, enums are held and released
without a runtime call, strings are held rather than copied, an array's header
is shared and its count inlined, and fixed-size boxes come from a free list
that no longer lives in thread-local storage. Indexing an array, or reading one
field of one element, no longer copies the array.

## Install

Download the `knvm-1.7.5-<host>.tar.gz` for your platform, put its `bin` on
PATH, and run `knvm install latest`. That is the only manual download; it
provisions everything else, LLVM bundle included.

Hosts: `aarch64-macos`, `x86_64-linux-gnu`, `x86_64-windows-msvc`.
