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

A struct cannot cross the `@Native`/`@Runtime` boundary. A `BridgeValue` is one
tag and one word of payload: a struct neither fits it nor has a shape for it,
and passing one needs an ABI decision — by value or by pointer, and who frees
the strings inside — that has not been made. Structs work fully on both engines;
only the crossing between them is unbuilt.

Three things enforce that, at descending distance from the user:

The **LLVM backend** refuses to emit any crossing whose signature mentions a
struct (`LlvmError::StructAtSeam`), which is what a user actually hits, at build
time, with the function named.

The **manifest** carries `BridgeValueTag::STRUCT`, which describes a type and
never travels. It exists because a manifest has a row for every function in the
program, and most never cross: a `@Runtime` function taking a struct and called
only from other `@Runtime` code is an ordinary program, and its row has to say
what its parameters are. No `BridgeValue` is built with this tag, and
`BridgeValue::decode` returns `None` for it — so one appearing on the wire is
rejected rather than guessed at.

The **VM** traps (`VmError::StructAtSeam`) if a struct reaches a `CallNative`
anyway. That means a module and a manifest that disagree, never a program that
merely type-checked.

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
