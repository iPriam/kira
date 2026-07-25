# Struct-by-value at the C seam — design

**Clang computes the aggregate ABI; Kira never classifies.** For each
`@FFI.Extern` whose signature names a C-layout struct, the backend generates a
small C translation unit that redeclares the struct and the real symbol and
wraps the call in a shim taking every aggregate by pointer:

```c
struct kira_ffi_agg_0 { int width; int height; /* … */ };
extern struct kira_ffi_agg_0 sapp_get_swapchain(void);
void kira_ffi_shim_7(struct kira_ffi_agg_0 *out) { *out = sapp_get_swapchain(); }
```

The managed clang compiles it. Everything Kira emits — the generated adapter,
the native call site, the VM host — then speaks only pointers and scalars, which
the existing seam already handles exactly.

The alternative is writing the classifier ourselves: eightbyte classification
for x86-64 System V, HFA/HVA detection and indirect-return rules for AArch64
AAPCS, plus the wasm32 rules, each provable only on a host of that
architecture. This machine can differential-test one of the three, so two thirds
of that code would ship asserted rather than proven. Delegating to the compiler
that defines the ABI removes the question instead of answering it badly.

`byval`/`sret` do not substitute for the shim. Both force the memory
classification, and the corpus needs the register cases: `MetalCGRect` is four
`F64`s, an AArch64 HFA returned in `v0`–`v3`, and `MetalNSPoint` is two.

## What crosses

An aggregate is a `@FFI.Struct { layout: c }` whose members are seam scalars or
nested aggregates of the same kind, to any depth. A member that is a callback,
an inline `@FFI.Array`, a `CString`, or a Kira heap type keeps its refusal — it
has no value the frontend can construct today, so admitting it at the seam would
buy nothing.

The struct stays a Kira struct on the Kira side. Only the marshalling is new:
the value is written into C-layout bytes for the call and read back out of them.

## Representation

`ForeignType` keeps its scalar-only vocabulary and its pinned tags. A position in
a signature becomes a `ForeignTypeSpec` — a scalar, or an index into the
program's aggregate table. Its serialized tag reuses the scalar tags 0–13 and
appends 14 for an aggregate, followed by the table index, so an old decoder
rejects a new module by unknown tag rather than misreading it.

An aggregate crosses the bridge as a pointer to `size` bytes of C-layout data
under a new `BridgeValueTag::AGGREGATE`. For a result the caller pre-writes the
tag and a pointer to its own buffer into the out slot, and the adapter fills it.
That is a semantic change to the adapter contract, so
`FOREIGN_ADAPTER_ABI_VERSION` goes to 2 and the marker symbol is renamed with it.

C layout is computed in `kira-runtime-abi` by the standard rule — each member at
the next offset meeting its alignment, the whole rounded up to the maximum
member alignment. The LLVM side needs no offset arithmetic: an LLVM struct type
built from the same member types has that layout already, so field access is
`StructGEP`.

## Proof

A parity test compiles a real C fixture whose functions take and return the same
structs by value, and runs it on vm, llvm, and hybrid, requiring byte-identical
output. Layout agreement with clang is proven separately by a fixture printing
`sizeof` and `offsetof` for each shape the test uses.
