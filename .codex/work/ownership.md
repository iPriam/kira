# Ownership: a static check, and why that is the whole feature

Ownership landed with **zero** IR, bytecode, VM, LLVM, hybrid, or wasm change.
That is not a shortcut taken — it is what the current type lattice makes true,
and the reasoning is worth keeping, because it stops being true the moment
arrays arrive.

## The argument

For scalars, `String`, and structs, a `move` and a `borrow` are both
**observationally identical to the deep copy the runtime already performs**.

- Reading a local copies it; a struct's fields are copied with it; a callee
  drops its own copy at frame exit. That is the existing `copy_value` /
  `drop_value` pair in `kira-vm-runtime/src/value.rs`, and both other backends
  mirror it.
- A caller that moved a value **can never look at it again** — that is exactly
  what the checker guarantees — so it cannot observe whether the callee aliased
  or copied.
- A `borrow` parameter is read-only, so the callee writes nothing back for the
  caller to observe either.

No program in this subset can tell the difference. Threading an unused mode
through fourteen files to express a distinction nothing can detect would be
cost with no claim attached, so the modes stop at the HIR.

## The one mode that is not free, and why it is refused

`borrow mut` is observable: a callee writing through the caller's binding is a
write the caller must see. Nothing below the analyzer carries it.

It is refused with `KSEM112` rather than accepted. Accepting it would not ship
an incomplete feature — it would ship a **silently wrong** one: the callee
would mutate its own copy and the caller would read the old value, with no
diagnostic anywhere. A rejected program is strictly better than a program that
computes the wrong answer.

This follows the oracle's own precedent for a reserved-but-unimplemented mode:
non-trivial `copy` is `KSEM116` there for exactly this reason, and returned
borrows are parsed and rejected pending lifetime validation. `KSEM112` was the
free code in the band.

**What it would cost.** By-reference passing in the VM is nearly the *absence*
of work — `Value::Struct` is already a heap handle, so a borrow is "do not copy
at load" — but the callee's frame exit drops every local, which would free the
caller's value. Fixing that needs per-function knowledge of which param slots
are borrowed: either a field on `FuncProto` (a KBC1 format change) or a new
append-only opcode voiding borrowed slots before each return. LLVM has real
destructors and needs the same answer; wasm's bump allocator never frees, so it
would work by accident there and prove nothing. That asymmetry is the reason to
do it deliberately rather than incidentally.

## What arrays need from this: nothing

The two predicates that decide every rule live on `Type`
(`kira-semantics-model/src/ty.rs`):

| Predicate | Question | Struct | Array (next) |
|---|---|---|---|
| `is_trivially_copyable` | does it reach an owned param without `move`? | no | no |
| `moves_on_bind` | does binding it consume the source? | **no** | **yes** |

A struct answers those two differently, and that split is the whole reason both
exist. `moves_on_bind` returns `false` for every type today — a struct
deep-copies, a `String` clones its bytes, so neither can alias and neither has
anything to enforce. The path through `Analyzer::apply_binding_move` is walked
on every `let` and does nothing.

That is deliberate. When `Type::Array` lands and answers `true`, implicit
move-on-bind, use-after-move, and the whole `KSEM107` band switch on with **no
new ownership code** — one arm in one predicate.

## What is not here, and why

- **`KSEM107`'s other two messages** (use-after-partial-move, and scope-exit
  with a field moved out) and **`KIR003`** have no trigger in this type
  universe. A partial move happens when a *field read* consumes its owner, and
  the oracle's predicate for that is `array | enum_instance | construct_any` —
  none of which exist yet. `let a = obj.field` on a struct or string field
  copies, so there is no partial move to track. Writing the tracking now would
  mean writing three diagnostics no program can reach and no test can pin.
- **`KIR002`** (the mid-IR alias overlap check) is likewise unreachable. Its
  live paths in the oracle are reborrow aliasing (`var r = t` where `t` is
  `borrow mut`) and move-only existentials. `borrow mut` is refused here and
  existentials do not exist, so there is no alias for the pass to find. When
  `borrow mut` lands, the reborrow rule and `KIR002` land with it — they are
  one feature, not two.
- **`KSEM117`** (non-Copy closure capture) needs closures.

The honest shape of this: the ownership *rules* are ported and enforced. The
two diagnostics that need aggregates, and the mid-IR pass that needs aliasing,
are waiting on the features that can produce the situations they describe —
not deferred as tail work.

## Reproduce rejections, do not fix them

Every case in `kira-semantics/src/lib.rs`'s ownership block is a program the
oracle's fail-corpus or harness pins, ported to this subset. Where the oracle
rejects, this rejects, including where a friendlier language would not: passing
a named struct of three `Int`s to an owned parameter needs `move`, because
"trivially copyable" is a property of the type constructor and not of what the
fields happen to be.
