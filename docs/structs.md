# Structs

A struct is a value, and every backend pays for that differently. The VM copies
a heap object on every read; the native backend copies an LLVM struct field by
field. Those are two mechanisms for one rule, which is why the rule is proven by
differential tests rather than asserted — `crates/kira-cli/tests/backend_parity/`
runs each case on VM, LLVM/native, and hybrid and requires identical output.

## The surface

Members are written with `let` or `var` and may carry a default. `=` is the
canonical field binder; `:` is accepted for the transition window and the two
may be mixed in one literal. Both spellings normalize to one node, so nothing
downstream can tell which was written.

```kira
struct Box {
    var origin: Vec3
    var label: String = "unnamed"
}

var b = Box { origin = v }   // `label` takes its default
b.origin.x = 100             // a nested write, in place
var copy = b                 // a deep copy: `copy.label = "x"` leaves `b` alone
```

Assigning to a place requires `var` at *every* step, not just the last. Writing
`b.origin.x` rewrites the contents of `origin`, so a `let` anywhere along the
path makes the write illegal.

## What is deliberate, not pending

**Field type order does not matter.** Struct collection declares every name
before resolving any field, so a field may name a struct declared later in the
file or in another file of the same package. A by-value cycle still has no finite
size; analysis detects it, reports `KSEM052`, and breaks the closing field to
`Error` so recursive layout, copy, and drop walks remain total.

**`print(someStruct)` is rejected** (`KSEM081`). What `print` renders for a
struct is pinned nowhere in the language corpus — no call site, no golden file —
so any text chosen here would be inventing language surface. The VM traps rather
than printing something made up, which is the runtime restating what analysis
already refused.

**A method is a function with a receiver.** A struct's methods are collected
into the same flat function table free functions live in, so nothing below
analysis learns one was written inside a struct. Non-mutating methods borrow the
receiver for reads. A method that writes `self` is marked mutating and the call
site writes its final receiver value back to the original mutable place. A body
may name a member bare, so `self.step` and `step` are the same read.

## The native seam

A struct crosses the `@Native`/`@Runtime` boundary as a **copy**, in either
direction, by value or by `borrow mut`.

A `BridgeValue` is one tag and one word of payload, which a struct fits neither
of, so what crosses is not the struct. The payload is a pointer to a tree of
nodes (`BridgeValueTag::NODE`, the shape callback state already crosses as), and
the tree carries the whole value however deeply nested. That answers both halves
of the ABI question the crossing once waited on: **by value**, and **the side
that reads the strings frees them**, exactly once, as it decodes. Neither engine
touches the other's heap, which is the only arrangement available: the VM holds
an index into its own storage, native holds a pointer to a box, and neither
means anything to the other.

`borrow mut` is that copy made twice. There is no pointer to lend, because the
caller's storage belongs to the other engine. The value goes over as a tree, and
the callee's final value comes back in the slot the argument arrived in, so the
argument array is written as well as read in both directions. The caller stores
what comes back into the place its own signature names, dropping what was there,
which is what an assignment does. A `borrow mut` parameter therefore behaves the
same across the seam as within one engine, which is what the parity tests
compare.

The manifest carries the mode per parameter (`Ownership::BorrowMut`), which is
what tells each side which slots to write and which to read back. It is
generated from the same IR both halves are compiled from, so the trampoline's
idea of which parameters are written through and the host's cannot disagree.

The **VM** still traps (`VmError::StructAtSeam`) if a struct reaches a
`CallNative` with no tree built for it, and reports
`VmError::MissingSeamWriteback` if a written-through parameter does not come
back. Both mean a module and a manifest that disagree, never a program that
merely type-checked.

Narrower things still cannot cross, each for a reason of its own rather than for
want of a layout: `Any` (`AnyAtSeam`), a C string, a task handle, a capture
cell, and a callback-state handle. A read-only `borrow` of a `String` is refused
too, because the callee frees every string it owns and a lent one would be freed
twice.

## Representation

The VM is **structurally typed**: a struct is a tuple of values on the same heap
as strings, sharing one pair of allocation counters, so `current == 0` at exit
proves both kinds balanced. Field names never reach the runtime — the compiler
resolved them to indices — so the bytecode module carries no struct table at
all. `NewStruct` carries its own arity, and `StoreField` carries the whole field
path, so a nested write mutates in place rather than rebuilding the enclosing
value.

The native backend uses a **real LLVM struct with real field layout**, not a
boxed or tagged value. That costs more to build — copies and drops are walked
field by field, inline — but `@FFI.Struct { layout: c; }` will need fields where
the target's ABI puts them, and a box would be the wrong foundation to put
underneath it.

Copies and drops mirror the VM instruction for instruction, because that is what
the parity tests compare. A local read copies; a field read copies the field out
*before* dropping the base, since the base owns the storage the field names; a
store drops what the location held after computing the new value, which is what
makes `s = s + "x"` work.

Defaults are a frontend concern only. After function signatures exist, analysis
resolves every field default once in the file where the field was declared. Its
qualified names therefore use that file's imports, a bare named function becomes
a typed function value there, and a genuinely undefined name is reported even
when no construction omits the field. Local scope stays empty, so a default
cannot capture a construction site's bindings. Every construction reuses the
resolved HIR expression, and `StructNew` still reaches lower layers with one
initializer per field in declaration order.

Defaults that recursively construct each other have no finite value and are
refused with `KSEM213` rather than recursing during analysis.

The wasm backend gives a struct an address in linear memory, with fields laid
out consecutively at natural alignment. Its heap never frees, so aliasing is
invisible for anything a program cannot mutate — which is why a `String` is
shared and costs nothing to "copy". A struct is not: `p.x = 1` writes through
the address, so it is deep-copied wherever the VM copies one, through a helper
generated per struct. Only the mutable spine is duplicated; the strings inside
are still shared. The Web target runs the same LLVM lowering the host does,
and `crates/kira-cli/tests/end_to_end/web.rs` compares a built module's output
against the VM's under node — a layout mistake shows up as a wrong value
rather than hiding.

## FFI boundary

`@FFI.Struct { layout: c; }` is recognized and uses the native backend's C
layout. `Type()` produces a zeroed value, while a struct literal initializes the
fields it names and zero-fills omitted fields that have no Kira default. Passing
aggregate values through every foreign signature remains a separate ABI surface;
unsupported positions are refused before code generation rather than assigned a
layout by guesswork.
