# FFI: the shape it has to have

Decisions taken 2026-07-17, before any of it is built. Nothing here is
implemented; this is the note that keeps the next session from re-deriving it or
building the wrong thing.

## The requirement

FFI works the same on VM, hybrid, and LLVM, with no `@Native` ceremony: an
extern is callable from any function, and a user never annotates to reach C.

`@Native`/`@Runtime` are orthogonal and stay what they are — a choice about
where *Kira* code runs, not about calling C. The struct-at-the-seam refusal is
about the Kira/Kira boundary and does not touch this.

## Who performs the call

`CallNative` already sets the precedent, and FFI copies it: **the VM performs no
call itself.** It hands the embedder safe Rust values through
`HostCapabilities` and pushes back what returns. For FFI the host — `kira`, the
runner — does the `dlopen`/libffi work through `kira-dynamic-ffi`. "Handled by
Kira" is satisfied by Kira's own host doing it; it is the same path `print`
already takes.

This is not tidiness. `kira-vm-runtime` and everything below it is the portable
core and must keep compiling for `wasm32-unknown-unknown`: no filesystem, no
process, no dynamic loading. Putting the call in the VM crate breaks the wasm
target.

## wasm does not do VM FFI

Decided: **the VM never does FFI on wasm.** A wasm VM's only host is JS, and
routing extern calls through it is not the answer.

Instead, FFI on wasm comes from the `@Native` half, where an extern becomes a
wasm import the page supplies. To keep that seamless — no annotation by hand —
a build option **forces FFI into `@Native`**: any function that reaches an
extern is promoted to `@Native` by the compiler, so the FFI lands in the half
that can perform it while the VM half still runs everything else.

The option is what makes "no `@Native` ceremony" and "wasm has no VM FFI" both
true at once. It is required on a wasm device and available elsewhere.

## What this needs that does not exist

**A hybrid split on the wasm device.** `--backend` and `--device` are now
independent axes and a device never overrides a backend — it only picks the
default for a command that named none, and an unbuilt pair is refused by name.
So `--backend hybrid --device wasm32` already parses and reaches the pipeline;
what it does not yet do is *work*, and it says so rather than quietly building
something else.

Making it work is the largest structural piece, and should be designed before
the extern surface. On the Web, `llvm` is the backend that serves the device —
the wasm backend is that device's code generator — so a hybrid split there means
a native half of wasm functions beside a VM half of bytecode, with the
interpreter compiled into the module. `kira-vm-runtime` already builds for
`wasm32-unknown-unknown`, so the VM half is wiring rather than a new engine.

**Promotion is a program transform, not a parse rule.** Forcing FFI into
`@Native` means resolving which functions transitively reach an extern and
rewriting their execution mode before the split. That belongs above semantics
and below the backends — the same place the split already reads annotations.
Note it is transitive: a `@Runtime` function calling a `@Runtime` function that
calls an extern must also move, or the boundary lands in the wrong place.

**Fixed-width integer types.** `@FFI.Struct { layout: c; }` needs `U8`, `I32`,
and friends; the v0 lattice has `Int`/`Float`/`Bool`/`String` only. The oracle's
FFI structs are `var a: U8`. This is a type-system expansion that comes *before*
the annotation, not after it.

## Order to build it

1. The extern declaration surface (`@FFI.Extern`), checked but not lowered.
2. The VM's foreign-call opcode plus its `HostCapabilities` method, and the host
   implementation over `kira-dynamic-ffi`. Desktop VM works here.
3. The LLVM path: a direct call to the symbol, and the link against the library.
   Desktop LLVM and hybrid work here.
4. Fixed-width integer types, then `@FFI.Struct { layout: c; }` with its
   zero-fill construction rule.
5. The wasm answer: the split on a wasm device, forced FFI promotion, and
   externs as imports.

Prove each step the way the rest of this workspace is proven — differentially,
in `crates/kira-cli/tests/backend_parity/`, not by assertion.
