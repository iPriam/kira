# Structs

A struct is a value, and every backend pays for that differently. The VM copies
a heap object on every read; the native backend copies an LLVM struct field by
field. Those are two mechanisms for one rule, which is why the rule is proven by
differential tests rather than asserted — `crates/kira-cli/tests/backend_parity.rs`
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

**A field may only name a struct declared earlier in the file.** This is not an
arbitrary scoping rule: a struct is a value type, so a struct that could reach
itself through its fields would have no finite size. Resolving in declaration
order makes the cycle unrepresentable rather than something to detect after the
fact — which is also what lets `StructTable::owns_heap` and the VM's recursive
free walk terminate without a visited set. A forward reference gets `KSEM051`,
which says to move the declaration rather than reporting an unknown type.

**`print(someStruct)` is rejected** (`KSEM081`). What `print` renders for a
struct is pinned nowhere in the language corpus — no call site, no golden file —
so any text chosen here would be inventing language surface. The VM traps rather
than printing something made up, which is the runtime restating what analysis
already refused.

**Struct methods parse but are reported** (`KPAR008`). The fields of a struct
carrying a method still parse, so one method does not cost the whole
declaration; but the method is never silently dropped, because a program missing
a method means something else.

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

Defaults are a frontend concern only. Analysis fills every omitted field with
its declared default, so `StructNew` always receives every field in declaration
order and nothing downstream knows defaults exist. A default is analyzed in an
empty scope, not the construction site's: it belongs to the declaration and must
not see whatever locals happen to be in scope where the struct is built.

## Not implemented here

The wasm backend does not lower structs yet (`WasmError::StructUnsupported`); a
program using one is refused rather than mislowered. Its strings live in linear
memory behind a bump allocator that never frees, so structs there need layout
and a generated deep-copy per type — the copy is what a program can observe,
since a struct is mutable and a shared one would alias.
