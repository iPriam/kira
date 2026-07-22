# kira-rusty

The Kira compiler and runtime in Rust — this is *the* Kira implementation, a
brand-new codebase. Kira has been rewritten from the ground up several times
over its life; this Rust repo is where it comes home.

Scaffolding phase: one crate per compiler/runtime concern, wired into a
layered dependency graph. The frontend, IR, VM, and backends are being
designed fresh from the language corpus, not carried over from any prior
implementation.

## Workspace layout

Crates live in `crates/`, organized into layers with no upward dependencies.

| Layer | Crates |
|---|---|
| 0 | `kira-core`, `kira-toolchain`, `kira-source`, `kira-diagnostics`, `kira-diagnostic-messages`, `kira-runtime-abi`, `kira-dynamic-ffi` |
| 1 | `kira-syntax-model`, `kira-lexer`, `kira-parser`, `kira-ksl-syntax-model`, `kira-ksl-parser` |
| 2 | `kira-semantics-model`, `kira-shader-model`, `kira-ksl-semantics`, `kira-semantics` |
| 3 | `kira-ir`, `kira-shader-ir`, `kira-hybrid-definition`, `kira-backend-api`, `kira-native-lib-definition` |
| 4 | `kira-glsl-backend`, `kira-wgsl-backend`, `kira-hlsl-backend`, `kira-msl-backend`, `kira-spirv-backend`, `kira-bytecode`, `kira-vm-runtime`, `kira-native-bridge`, `kira-hybrid-runtime`, `kira-debug`, `kira-llvm-backend` |
| 5 | `kira-manifest`, `kira-project`, `kira-package-manager`, `kira-build-definition`, `kira-main` (Rust embedding surface: staticlib/cdylib/rlib) |
| 6 | `kira-program-graph`, `kira-hybrid-main` (hybrid embedding surface: bytecode half plus a loaded native half) |
| 7 | `kira-build` (frontend driver, library builds, generated Rust wrapper crates) |
| 8 | `kira-instruments`, `kira-linter`, `kira-doc`, `kira-app-generation`, `kira-live` |
| 9 | `kira-cli` (binary `kirac`) |
| tests | `kira-export-consumer` (a Rust program consuming a Kira library, end to end, on each of the three engines) |
| runners | `kira-desktop-runner` (binary `kira-desktop-runner`) |
| tools | `kira-bootstrapper` (binary `kira`), `kira-knvm` (binary `knvm`), `kira-devflow` (binary `devflow`) |

`kira-lsp` is the language-server surface over the salsa frontend.

## Building

```sh
cargo build
cargo clippy --workspace
```

The VM-hot crates (`kira-vm-runtime`, `kira-bytecode`) are compiled with
`opt-level = 3` even in the dev profile: a debug interpreter is 4–11× slower,
and the dev snapshot is what `kira run` uses for interactive work.

## Structs

A `struct` is a non-inheriting value shape. Members are written with `let` or
`var` and may carry a default, which fills the member wherever a literal leaves
it out:

```kira
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

struct Box {
    var origin: Vec3
    var label: String = "unnamed"
}

let v = Vec3 { x = 1, y = 2, z = 3 }
var b = Box { origin = v }   // `label` takes its default
b.origin.x = 100             // a nested write lands in place
```

`=` is the canonical field binder; `:` is still accepted, and the two may be
mixed in one literal. A struct is a **value**: `var copy = b` copies it deeply,
strings included, so writing to the copy never disturbs the original.

Two edges are deliberate rather than pending:

- **`print(someStruct)` is rejected.** What `print` renders for a struct is not
  pinned anywhere in the language corpus, and inventing a format here would be
  inventing language surface. Print a struct's fields until it is settled.
- **A struct cannot cross the `@Native`/`@Runtime` boundary.** It does not fit
  a `BridgeValue`, and passing one needs an ABI decision — by value or by
  pointer, and who frees the strings inside — that has not been made. Structs
  work on both engines; only the crossing is unbuilt, and a build that would
  need one says so. See [docs/structs.md](docs/structs.md).

A struct may declare **methods** alongside its members. A method is an ordinary
function that happens to have a receiver, so it takes a slot in the same
function table every free function does — nothing below analysis learns it was
written inside a struct. The receiver arrives by value, like any other
parameter, so writing to `self` inside a method leaves the caller's value
alone. A method's body may name a member bare (`self.x` and `x` are the same
read).

## Classes

A class is a struct that inherits. It has everything a struct has — fields with
defaults, methods, value semantics — plus `extends`, `override`, and
parent-qualified access:

```kira
class Account {
    var balance: Int = 100
    let rate: Int = 2

    function gross() -> Int { return self.balance * self.rate }
}

class Savings extends Account {
    override let rate = 5                       // the same slot, a new default

    function bonus() -> Int {
        return Account.gross() + self.balance   // this is how "super" is spelled
    }
}

print(Account().gross())    // 200
print(Savings().gross())    // 500 — the inherited method reads the new default
```

A class is **built by calling it**, not with a `{ }` literal. Arguments fill
the fields that declare no default, in flattened order; every field with a
default takes it. A class is a **value**, like a struct: `var b = a` copies, and
mutating the copy leaves the original alone.

`extends` takes a comma-separated list, and may name a `struct` as readily as a
class. Two parents declaring the same name keeps **both** — they are separate
fields and separate methods — so the bare name is ambiguous (`KSEM067` for a
method, `KSEM068` for a field) and qualifying it says which was meant.

Nothing below semantics learns classes exist. A class flattens into an ordinary
struct, and each class gets its **own copy** of every method it inherits, with
`self` typed as that class. That is worth stating plainly because it buys the
one thing inheritance usually costs: since `self` is always statically the
concrete class, an inherited method calling `self.m()` reaches the override, and
it does so identically on the VM, native, hybrid, and wasm — with nothing
dispatched at run time. The reference implementation has a live vm/llvm
divergence on exactly this shape and steers its corpus around it; here it is a
tested case.

One edge is deliberate rather than pending:

- **There is no subtyping.** A `Savings` is not assignable to an `Account`
  binding or parameter (`KSEM063`). Nothing in the language corpus binds a
  derived instance to an ancestor type, so nothing pins what it would mean —
  and admitting it is precisely what would reintroduce the dispatch question
  the flattening removes. Every class instance's static type is its dynamic
  type.

See [.codex/work/classes.md](.codex/work/classes.md) for the design, and
[examples/classes/classes.kira](examples/classes/classes.kira) for a tour.

## Arrays

An array is a shared, growable, heap-backed sequence, written `[T]`. Its whole
surface is two members — `.append(v)`, which grows it in place, and `.count`, a
property with no parens — plus `xs[i]` to read and write elements:

```kira
let xs = [1, 2, 3]           // full the moment it exists; commas optional
var ys: [Int] = []           // the universal idiom: an empty literal, grown
for i in 0..5 { ys.append(i * i) }
ys[1] = 99                   // an index write lands in place
print(ys.count)              // a property, never `.count()`

var grid: [[Int]] = [[1, 2], [3, 4]]
grid[1][1] = 77              // a write walks as deep as the path goes
```

An out-of-range or negative index is a **runtime trap**, not a compile error —
an index is rarely a constant, so a static check would reject working programs.
A negative index and one past the end are *different* traps, because they are
different mistakes.

The value semantics are the mirror image of a struct's. A struct copies on
binding, so a copy is independent. An array is a **handle**: reading one *out*
of a place (an element, a returned value) copies it, so what you read cannot be
perturbed afterwards — but the array itself aliases, which is why the ownership
checker **moves it on binding** (`let alias = xs` ends `xs`). There is no array
clone: `copy xs` is `KSEM116`. Independent arrays come from building with
`append`, or from copying a struct that owns one — which deep-copies the array
field rather than sharing the handle, the question the whole design turned on.

Two edges match the struct ones, one for the same reason and one not:

- **`print(someArray)` is rejected (`KSEM081`).** Same as a struct: no corpus
  call site pins a separator or a bracket, so a format here would be invented
  surface.
- **An array cannot cross the `@Native`/`@Runtime` boundary yet.** Unlike a
  struct, this is a *gap, not a decision* — the language does let an array
  cross; what is missing is the ownership answer at the seam (who frees the
  elements, what a native callee growing the array means for the other half). A
  build that would need the crossing says so rather than guessing.

See [.codex/work/arrays.md](.codex/work/arrays.md) for the design, and
[examples/arrays/arrays.kira](examples/arrays/arrays.kira) for a tour.

## Enums

An `enum` is a value that is one of a fixed set of named variants, each
optionally carrying a single payload. Variants are separated by newlines or
spaces — never commas.

```kira
enum Color { Red Green Blue }

enum Message {
    Empty
    Text(String)                              // a payload
    InvalidFormat: String = "not that format" // a payload with a default
}
```

A variant is written with a **leading dot** — `.Red`, `.Text("hi")` — and what
it resolves against is the type *expected* at that position: a `let` annotation,
a parameter, a return type, a struct field, or the other side of a comparison.
So `.Red` alone is not a value; `let c: Color = .Red` is. A dot against a
non-enum type, or in a position with no expected type, is refused (`KSEM119`).

`==` and `!=` compare **discriminants**, so `c == .Red` asks which variant `c`
is. Reading a payload back out is [`match`](#match).

```kira
function rank(c: Color) -> Int {
    if c == .Red { return 1 }
    if c == .Green { return 2 }
    return 3
}
```

Like an array, an enum is a heap value that **moves on binding** (`let b = a`
consumes `a`) and is **not** trivially copyable (a named enum needs `move` into
an owned parameter; a fresh `.Variant` needs nothing). Three edges match the
struct/array ones:

- **`print(someEnum)` is rejected (`KSEM081`).** No corpus site pins a
  rendering, so a format here would be invented surface.
- **An enum cannot cross the `@Native`/`@Runtime` boundary.** Like a struct, it
  is a tagged value with no one-word form, and how it would cross is undecided.
- **A payload may be `Int`, `Float`, `Bool`, `String`, or another enum.** A
  struct or array payload is refused (`KSEM118`): the runtime box carries one
  type-erased word, which an aggregate has no form in yet. A nested enum is a
  handle, so it fits that word — and it has to, because a `Result`-shaped
  value's `Error` variant carries the failure enum.

See [.codex/work/enums.md](.codex/work/enums.md) for the design, and
[examples/enums/enums.kira](examples/enums/enums.kira) for a tour.

### Generic enums

An enum may take **type parameters**, which is what lets the standard library
declare `Result` once:

```kira
enum Result<Value, Failure> {
    Ok(Value)
    Error(Failure)
}
```

That declaration names no type. Each written instantiation does:
`Result<Int, AppError>` substitutes the arguments into the variants and produces
an ordinary enum whose `Ok` carries an `Int` and whose `Error` carries an
`AppError`. Two writings of the same arguments name the same type; different
arguments name different types and do not assign to each other.

Substitution happens during analysis, so nothing below semantics learns that
generics exist — no opcode, no IR node, no wire format. A generic instantiation
behaves exactly like the enum you would have written by hand: same leading-dot
construction, same `match`, same move semantics. A monomorphized `Result` is
`Result`-shaped, so [`attempt`/`try`](#attempt-try-and-handle) unwrap it with
nothing added.

The **enum is the only declaration that takes type parameters**. A generic
`struct`, `class`, or `function` is refused by name (`KPAR047`): the reference
corpus contains exactly one generic declaration, and it is `Result`, so anything
else would be invented surface. The other edges are typed diagnostics too — an
arity mismatch is `KSEM174`, a generic enum written bare is `KSEM172`, type
arguments on a non-generic type are `KSEM173`, and a template that grows its own
argument without end is `KSEM175` rather than a stack overflow.

See [examples/generics/generics.kira](examples/generics/generics.kira) for a
tour.

## Ownership

Kira owns by default, and says so at the call site. A plain parameter
**consumes** the value it is given, so passing a *named* non-trivial value to
one must write `move`:

```kira
function consume(v: Vec3) -> Int { return v.x + v.y + v.z }
function sum(v: borrow Vec3) -> Int { return v.x + v.y + v.z }

let v = Vec3 { x = 1, y = 2, z = 3 }
print(sum(v))            // a borrow reads and gives back; `v` survives
print(consume(move v))   // this takes `v` away
print(v.x)               // KSEM107: `v` was moved and is no longer available
```

There are five modes and no others: `owned` (the default), `borrow`,
`borrow mut`, `move`, and `copy`. All four written spellings are **contextual
identifiers**, not reserved keywords — a variable named `move` still parses,
because a `move` is only an operator when an operand follows it.

Which values need `move` is one predicate: **trivially copyable** covers
`Void`, `Int`, `Float`, and `Bool`. A `String` is not trivially copyable (it
owns its bytes) and neither is a struct — so both need `move` into an owned
parameter. A temporary never does: `consume(Vec3 { … })` binds nothing, so
there is nothing to lose track of.

A struct nonetheless **copies when bound**: `var w = v` deep-copies, and `v`
stays live. Needing `move` and moving on bind are different questions, and a
struct answers them differently. An **array** and an **enum** are the types that
answer the second one `yes`: their bindings alias where a struct copies, so
binding one moves it (see [Arrays](#arrays), [Enums](#enums)).

`copy` is reserved but has no clone semantics: `copy` on anything non-trivial
is `KSEM116` rather than a deep copy invented here. Borrow a value, move it, or
build a new one.

Two edges are deliberate:

- **`borrow mut` is refused (`KSEM112`).** It is the one mode that is
  observable at run time — a callee writing through the caller's binding — and
  no backend carries the by-reference calling convention yet. Accepting it
  would not be an incomplete feature; it would silently compute wrong answers,
  because the callee would write to a copy the caller never sees. Take the
  value with `move` and return the updated one.
- **Ownership costs no backend anything.** For today's types a `move` and a
  `borrow` are both indistinguishable from the deep copy the runtime already
  performs — a caller that moved a value can never look at it again, which is
  exactly what the checker guarantees — so the whole subsystem is a static
  check. See [.codex/work/ownership.md](.codex/work/ownership.md).

## Parameter defaults

A parameter may declare a default with `=`, and a call may then omit its
argument:

```kira
function step(base: Int, by: Int = 1, tag: Int = 100) -> Int {
    return base + by + tag
}

print(step(1))              // 102 — both defaults filled
print(step(1, 5))           // 106 — one default filled
print(step(base: 1, tag: 9)) // 11 — a labeled call omits the middle default
```

A positional call fills omitted **trailing** arguments; a labeled call may omit
any defaulted parameter, middle ones included. Every call path fills the same
way — a free function, a method, and a construct-family modifier alike. A
parameter with no default is still mandatory: omitting it is `KSEM062`, exactly
as before.

The default is resolved **once**, in the file the function was declared in — so
it may name that file's helpers regardless of where the call is written — and
the resulting value is reused at every call that omits the argument, mirroring a
struct field default. Two defaults that fill each other through the call graph
(`f(x = g())` where `g(y = f())`) have no finite value and are refused with
`KSEM240`.

## Loops

`while` tests before each iteration. `for` walks a **half-open** integer range:
the lower bound is included and the upper one is not, so `for i in 0..5` sees
`0 1 2 3 4` and `for i in 5..5` never runs at all. `..` already means "up to
but excluding", which is why there is no separate `..<`.

```kira
for i in 0..5 { print(i) }

let lo = 2
let hi = 6
for i in lo..hi { print(i) }    // bounds are expressions, evaluated once
```

A range is written only in a `for` header — `..` is not a value operator, so
`let r = 0..4` is rejected rather than producing a range object.

A `for` also walks an **array**: `for x in xs` binds each element in turn. It
only reads, so `xs` is still usable afterwards, and the loop variable is a
*copy*, so writing to what it names cannot perturb the iteration:

```kira
let xs = [10, 20, 30]
var total = 0
for x in xs { total = total + x }   // xs survives; x is an immutable copy
```

The loop variable is a fresh **immutable** binding on each iteration, scoped to
the body: assigning to it is the same error assigning to any `let` is, and it
does not outlive the loop.

`break` leaves the innermost enclosing loop and `continue` skips to its next
iteration; both work in `while` and `for`, and one written outside a loop is
reported rather than ignored. A `for` is rewritten into a `while` during
analysis, so every backend compiles one loop shape rather than two — and
`continue` still advances the loop, because the rewrite steps the cursor before
the body rather than after it.

## Switch

`switch` dispatches on a subject by comparing it to each `case` label with
`==`, so a label may be any type `==` accepts against the subject: `Int`,
`Float`, `Bool`, or `String`. Arm bodies are braced blocks, and the `:` after a
label is optional.

```kira
switch i % 3 {
    case 0 { print("zero") }
    case 1 { print("one") }
    default { print("many") }
}
```

The subject is evaluated once. Labels are evaluated lazily in source order, so
a label after the matching one never runs. There is **no fallthrough**: the
first matching arm runs and control resumes after the switch.

`default` is optional and need not come last. A `switch` that matches nothing
and has no `default` simply does nothing — there is no exhaustiveness check,
and a repeated label is legal, with the first match winning.

A `switch` is a statement, not an expression: an arm that wants to produce a
value assigns to a `var` or returns. **`break` inside an arm belongs to the
enclosing loop, not to the switch** — a switch is not a loop, so a `break` in
one that no loop encloses is reported.

## Match

A `match` dispatches on an **enum's variant**. Arms are written with an arrow,
and take either a single statement or a block. Variants are **unqualified** —
the subject's type already says which enum they belong to, so it is `Light`,
never `Shade.Light` or `.Light`.

```kira
function rank(s: borrow Shade) -> Int {
    match s {
        Light -> return 1;
        Mid -> return 2;
        Dark -> return 3;
    }
}
```

A parenthesized name **binds the variant's payload**, and is visible only inside
its own arm — so two arms may bind the same name to different payloads. The
binding is an owned copy that outlives the enum it was read from, and it is
immutable.

```kira
match note {
    Tag(text) -> { print(text) }
    Rank(value) -> { if value > 10 { print("high") } }
    Blank -> { print("none") }   // an arm may ignore a payload it does not need
}
```

Unlike a `switch`, a `match` is **checked**: every variant must be covered
(`KSEM129`), and a variant matched twice is reported (`KSEM127`). That is the
whole reason the two constructs are separate — a `switch` label is an arbitrary
expression, so there is no variant set to be exhaustive over. Neither check
applies to `switch`, and a `match` subject that is not an enum is refused
(`KSEM125`).

Because coverage is checked, an exhaustive `match` whose arms all return is
itself a **definite return** — which is why `rank` above needs no trailing
`return`. As in a `switch`, `break` inside an arm belongs to the enclosing loop.

## Attempt, try, and handle

`try` unwraps a **`Result`-shaped** value: an enum with an `Ok` variant and an
`Error` variant whose payload is the failure enum. On `Ok` the body carries on
with the unwrapped value; on `Error` control leaves the body for the `handle`
arm naming that failure.

```kira
enum ClampError { TooSmall TooBig }

enum ClampOutcome {
    Ok: Int
    Error: ClampError
}

function process(n: Int) -> Int {
    attempt {
        let v = try clamp(n)
        return v * 2
    } handle {
        TooSmall { return 0 - 1 }
        TooBig { return 0 - 2 }
    }
}
```

"Result-shaped" is **structural, not nominal** — any enum with that `Ok`/`Error`
pair works, which is why `ClampOutcome` is declared right there. A handler arm is
spelled `Variant { … }` with **no arrow**, unlike a `match` arm, and a
parenthesized name binds the failure's payload exactly as a `match` binding does.

`try` is a **keyword**, never `?`, and it is accepted in exactly one position:
as the whole initializer of a `let` directly inside an `attempt` body. Anywhere
else — outside an `attempt`, or nested in a larger expression — is refused
(`KSEM137`), because the corpus writes only that one spelling and nothing pins
what `g(try f(), try h())` would mean.

The handlers are checked the way a `match` is. Every reachable failure variant
must be handled (`KSEM139`), an arm naming something that is not a variant of
the failure enum is reported (`KSEM140`), and a failure handled twice is
reported (`KSEM142`). Because all the arms route the same value, **every `try`
in one `attempt` must fail with the same enum** (`KSEM141`); a `try` on
something that is not `Result`-shaped is `KSEM138`, and an `attempt` with no
`try` at all is `KSEM143`.

Statements after a `try` run only when it succeeded, so they nest into the
success branch. That is what makes `process` above a definite return with no
trailing `return`: the failure test's two branches both return.

See [.codex/work/attempt.md](.codex/work/attempt.md) for the desugar.

## Fixed-width scalars

`I8`, `I16`, `I32`, `I64`, `U8`, `U16`, `U32`, `U64`, `F32`, and `F64` name
integer and float types alongside bare `Int` and `Float`. They are **spellings,
not representations**: every integer type is one 64-bit two's-complement value
at run time and every float type one 64-bit IEEE-754 value, so `I32` does not
allocate a narrower box.

A spelling decides exactly two things.

**Distinctness.** Two *written* widths must match exactly — a `U8` does not flow
into an `I64`, and `u8Value + i64Value` is `KSEM071` — because the language has
no implicit widening. Bare `Int` and `Float` are the exception: each is a
wildcard matching any width in its kind, which is what lets an integer literal
be written at any width with no conversion rule.

```kira
let small: U8 = 5     // a literal is plain `Int`, so it fits any width
let plain: Int = small // a width flows into the wildcard
let back: U8 = plain   // and back out of it
```

That makes assignability deliberately **non-transitive**: `U8` → `Int` and
`Int` → `I64` both hold while `U8` → `I64` does not. The wildcard is what a
literal needs and the exactness is what a width means.

**Signedness of `/`, `%`, and the four orderings.** A `U` spelling selects the
unsigned form of those six operators and only those six — `+`, `-`, `*`, and
`==` are bit-identical under either signedness, so they need no unsigned twin.
Read as a `U64`, `-1` is `18446744073709551615`:

```kira
let big: U64 = -1
let two: U64 = 2
print(big > two)   // true — unsigned; signed this would be false
print(big / two)   // 9223372036854775807 — signed this would be 0
```

**The left operand decides.** When one side is a plain `Int`/`Float` and the
other carries a width, the operation takes its type — and so its signedness —
from the left side alone. Mixing is allowed, but it is not symmetric:

```kira
let neg: Int = 0 - 10
let three: U8 = 3
print(neg / three)   // -3    — LHS is plain `Int`, so this is a signed divide
print(three / neg)   // 0     — LHS is `U8`, so this one is unsigned
```

Two *different* written widths agree on nothing: `u8Value + i64Value` is a type
error, because the language has no widening.

What a width does **not** do is narrow arithmetic. A `U8` sum of `250` and `10`
is `260`, not `4`: every operation wraps at the representation's 64 bits, never
at the written width. Narrowing is behavior the language does not define, so
this port declines to invent it.

`Byte` is not a builtin. The language spells it as an alias — `type Byte = U8` —
which is why it behaves exactly as `U8` does, unsigned division included. There
is no `I128`, no `U128`, and no `Char`.

Six opcodes carry the signedness to the VM (`DIV_UINT`, `REM_UINT`, `LT_UINT`,
`LE_UINT`, `GT_UINT`, `GE_UINT`, appended after `ENUM_PAYLOAD`); native code
lowers them to `udiv`/`urem` and the unsigned `icmp` predicates, and wasm to
`i64.div_u`/`i64.rem_u` and the `_u` comparisons.

## Conditionals and bitwise operators

`c ? a : b` is the language's only expression that is **control flow**: exactly
one branch is evaluated. That matters whenever a branch can fail, and it is why
no backend lowers this to a `select` instruction — a select evaluates both.

```kira
function safeDivide(numerator: Int, divisor: Int) -> Int {
    return divisor == 0 ? 0 : numerator / divisor
}
```

The condition must be `Bool`; there is no truthiness. Both branches must agree
on a type, with a bare literal acting as the wildcard it is elsewhere, so
`flag ? 0 : u8Value` is a `U8`. Two `Void` branches are rejected: a `? :` has to
leave a value behind. The form nests to the right, so `a ? 1 : b ? 2 : 3` reads
as a chain of tests without parentheses.

`& | ^ ~ << >>` act on the raw 64 bits of an integer. `&`, `|`, `^`, and `<<`
need no unsigned twin — a bit has no sign — but `>>` does, and takes its form
from the **left** operand's spelling: signed propagates the sign bit, unsigned
fills with zeros.

```kira
let signed: I64 = -1
var unsigned: U64 = 0
unsigned = unsigned - 1   // the same 64 bits
print(signed >> 60)       // -1
print(unsigned >> 60)     // 15
```

A shift count is taken **modulo 64** on every backend rather than trapping, so
`1 << 64` is `1`. Nothing is undefined; LLVM's shifts would be poison here, so
the native backend masks the count explicitly to match the VM and wasm.

**The precedence ladder is exactly C's**, loosest to tightest:

```
? :   ||   &&   |   ^   &   == !=   < <= > >=   << >>   + -   * / %
```

Two rungs surprise people anyway, because Go and Swift moved them and memory
tends to follow the newer language. The bitwise operators bind **looser than
equality** — C's classic wart — so `flags & 8 == 8` groups as `flags & (8 == 8)`,
a type error here since `&` wants two integers. Write `(flags & 8) == 8`. And
the shifts bind **tighter than the orderings but looser than `+`**, so
`1 + 2 << 3` is `(1 + 2) << 3`, which is 24.

Seven opcodes carry the operators to the VM (`BIT_AND`, `BIT_OR`, `BIT_XOR`,
`SHL`, `SHR_INT`, `SHR_UINT`, `BIT_NOT`, appended after `GE_UINT`). The
conditional needs **no opcode at all**: it compiles to the jump-and-patch shape
`&&`/`||` already use, lowers to a branch and a phi natively, and is wasm's
value-typed `if`.

## Imports

A program is a directory of `.kira` files. One of them declares `@Main` and is
the entry file; the rest are modules, and an `import` is what pulls one into
the program:

```kira
import geometry as Geo
import shapes.Rect as Rect

@Main
function main() {
    let origin: Geo.Point = Point { x: 0, y: 0 }
    print(Rect.area(origin))
    return
}
```

A module name is a *name*, not a path: no slashes, no extension, no `..`. A
single segment is a sibling file (`geometry` is `geometry.kira`), and a dot is
a directory separator (`shapes.Rect` is `shapes/Rect.kira`), both resolved
against the entry file's directory. An import naming no readable file is
`KSEM032`.

Imports are **file-scoped**. An import written in `main.kira` says nothing
about `geometry.kira`: every file writes the imports it needs, and a module
that wants `shapes.Rect` imports it itself. Referring to a namespace root this
file did not import is `KSEM027`, even when a sibling imported it.

What an import binds is a **namespace root** — the `as` name, or the module's
last path segment when no alias is written. The root is a qualifier for names
the module declares: `Geo.Point` as a type, `Rect.area(p)` as a call. It is not
a visibility gate. A package's top-level declarations are visible bare
everywhere in the package, so `Point` and `area(p)` work too; the qualifier is
how a reader sees where a name came from.

Two modules that import each other are a legal program. Loading is
visited-set-guarded, so a cycle terminates and each file lands in the program
once — there is no import-cycle diagnostic, because the reference
implementation accepts these and rejecting them would break working programs.

`import Foundation` parses and resolves like any other import, and reports
`KSEM032` here: there is no Foundation package in this repo yet.

Imports are resolved entirely in the frontend. By the time the IR exists a
program is one flat list of functions, so no backend — VM, LLVM, hybrid, or
wasm — learns that a program was ever more than one file.

## Type aliases

`type Name = Target` binds a name to a written type. It is a **spelling, not a
new type**: `Count` and `Int` below are the same type, so a value of one goes
anywhere the other does and no backend ever learns the alias existed — analysis
resolves it away, which is why the feature costs no opcode and no lowering.

```kira
type Count = Int
type Buffer = [Count]
type Matrix = [Buffer]
```

The target is an ordinary type reference, so an alias names anything a type
position can: a builtin, a struct, an enum, an array of any depth, or another
alias. Aliases chain in either declaration order — `Matrix` above would resolve
just as well written first — because resolution is lazy rather than
order-dependent the way a struct field's type is.

Two ways to get one wrong are reported. A chain that comes back to where it
started has no type at the end of it and is refused (`KSEM157`) rather than
resolved. And a name that already means something — a builtin, a struct, an
enum, or an earlier alias — may not be claimed (`KSEM130`): a silently-ignored
`type Int = Float` would keep type-checking as `Int` and give a wrong answer
instead of an error.

## Closures

A closure is a function value, written `{ params in body }`; its type is written
`(A, B) -> R`. A closure never annotates its parameters, so a literal only makes
sense where a function type is already expected — an annotation, an argument, a
field, or a `return`. Nowhere else says what the parameters are, and guessing is
refused (`KSEM134`) rather than attempted.

```kira
function apply(f: borrow (Int) -> Int, x: Int) -> Int { return f(x) }

function makeAdder(step: Int): (Int) -> Int {    // a result type after `:`
    return { value in return value + step }
}

let scale: (Int) -> Int = { v in return v * 2 }  // one parameter
let pick: (Int, Int) -> Int = { a, b in return a }
let now: () -> Int = { in return 1 }             // zero parameters: a bare `in`

each { value in print(value) }                   // trailing: the last argument
graphics.runWithConfig(2) { frame in ... }       // …after the other arguments
```

A function type is written after `:` on a declaration that returns one, because
`->` would be ambiguous with the function type's own arrow. Both spellings stay
valid for every other result type.

A top-level function name is a function value too. An expected function type
checks its parameter and result types exactly; without an expectation, the
function's signature supplies the type. A mismatch is `KSEM212`. Named functions
use the same synthesized representation and dispatcher as closure literals, with
an environment-free adapter as their dispatcher arm.

**A closure costs no opcode, no IR node, and no backend code.** A function type
becomes a synthesized struct — a tag plus the captures of every literal of that
type — a literal becomes a lifted top-level function plus a value of that
struct, and a call through a closure value becomes a call to a synthesized
dispatcher that branches on the tag. Every construct that reaches the IR is one
every backend already ran, which is why the feature lands on VM, LLVM/native,
hybrid, and wasm at once and adds a path to none of them.

**A capture is a copy, and must be an immutable scalar.** A closure copies what
it reads from around it at the moment it is built:

- A `let` of `Int`, `Float`, or `Bool` is captured by value.
- A `var` is **refused** (`KSEM117`). The reference implementation *borrows* a
  mutable capture so a write inside the closure is visible outside; nothing in
  this runtime shares storage — every value is copied on read, on every backend
  — so copying it would run and quietly give a different answer. This is the one
  piece of the reference behaviour that is not reproduced, and it is refused
  rather than approximated.
- A `String`, struct, array, or enum is refused by the same code: those own heap
  storage, and `isTriviallyCopyable` admits only the scalars.

Captures nest and shadow the way bindings do. A closure two deep captures an
outer binding *through* the closure between it rather than reaching past it, a
parameter shadows a capture of the same name, and a name is readable as a
capture before an inner `let` of that name rebinds it.

A closure lifted out of a method has no `self`, so a bare field name in its body
is an undefined name rather than a silent capture.

See [.codex/work/closures.md](.codex/work/closures.md) for the design, and
[examples/closures/closures.kira](examples/closures/closures.kira) for a tour.

## Library packages

A package declaring `kind = .Library` in its `package.kira` builds to a linkable
artifact instead of a program, and needs no `@Main`:

```kira
Package uifoundation {
    let version = "0.1.0"
    let kind = .Library
}
```

```sh
kirac check uifoundation.kira                      # clean with no @Main
kirac build --backend vm uifoundation.kira         # a .kbc plus a Rust crate
kirac build --backend llvm uifoundation.kira       # a static archive, no C main
kirac build --backend hybrid uifoundation.kira     # both halves plus a manifest
kirac run uifoundation.kira                        # refused: no entrypoint
```

`kirac` finds the manifest by walking up from the source file, so the package is
what decides this and there is no flag that could disagree with it. A file with
no `package.kira` above it is a program, which is why a bare `.kira` handed to
the compiler still reports a missing `@Main`.

The entrypoint rule lives in analysis, above the backend split, so it is one
rule rather than three: an application must declare exactly one `@Main`
(`KSEM011`), and a library must declare none (`KSEM158`). Running a library is
refused identically on all three backends, because there is nothing backend-
specific about a package having no way in.

### What a library build refuses, and what would lift it

Three things, each refused by name with its reason and the change that would
build it. None is a silent gap, and none is discovered at run time.

**A library as a wasm module artifact.** A Web build emits one self-contained
module entered at `main`, and the string and allocator contract across a wasm
module boundary is undesigned — so there is no answer yet to who allocated a
string a JS host is holding. Lifting it means deciding that contract and
emitting one wasm export per Kira export through the same backend that emits
`main` today. What works instead is the other wasm consumer — a Rust program
that embeds the library and is *itself* compiled to `wasm32-unknown-unknown`,
which the VM engine's generated crate supports because everything under it
does.

**An `@Export` that is also `@Native`, and a `@Native` function that calls a
`@Runtime` one** — both on the hybrid engine, both by function name. A consumer
always enters the bytecode half, and a handle is a root into that half's heap,
so machine code cannot mint one; and a library instance owns a heap and is
entered through a mutable borrow, so it cannot be re-entered from inside a call.
Neither is a missing feature: an *application* built with `--backend hybrid`
calls in both directions, and giving that up is the whole of what a library
gives up.

`@Native` on its own is refused by no engine. `--backend vm` compiles every
function to bytecode whatever it was annotated with — which is what makes `vm`
and `llvm` comparable on any program — so nothing native executes on a pure VM
and there is nothing to prevent.

**Two Kira native libraries in one binary.** Both archives carry the `kira_rt_*`
runtime, so linking both fails with a duplicate-symbol error — loud, at link, by
symbol name, which is what makes it an acceptable v1 answer rather than a trap.
The fix is per-library runtime prefixing (`kira_rt_*` becoming
`kira_rt_<library>_*`), and a host that `dlopen`s the shared form is already
isolated by `RTLD_LOCAL`.

### `@Export`: the consumer-facing surface

`@Export` names the functions a consumer may call. It is **new Kira design** —
the oracle has no library-export concept — and it is bare: no arguments, no
block, no symbol override. The consumer's name is derived by snake_casing, so
`makeButton` is reached as `make_button`.

```kira
@Export
class Button {                                  // handle-eligible
    var title: String = ""
    var width: Int = 120
    function label() -> String { return self.title }
}

@Export
function makeButton(title: String) -> Button {  // a handle out
    var b = Button()
    b.title = title
    return b
}

@Export
function buttonWidth(b: Button) -> Int { return b.width }
```

`@Export` on a class means only that its instances may cross as opaque handles;
it exports no method. Functions are the exported surface, so an author wraps a
method in an exported function.

The boundary refuses what it cannot carry, by name and with a reason, rather
than inventing a representation: an array (`KSEM160`, who frees the elements is
undesigned), a struct or an enum by value (`KSEM161`/`KSEM162`, neither fits one
tag and one word), a function value (`KSEM163`), a class that is not itself
`@Export` (`KSEM164`), and a `move` or `borrow mut` parameter (`KSEM165`,
the boundary's ownership is fixed per type). `@Export` in an application package
is `KSEM159`, a payload after it is `KSEM166`, a method export is `KSEM167`, and
two exports colliding after snake_casing is `KSEM168`. `@Export` on a `struct`
is refused in the parser (`KPAR043`), which points at the `class` that would
work. All of them are checked in
analysis, above the backend split, so three engines cannot grow three opinions
about what an export is.

The wire format the surface travels in is in place: a compiled module carries an
appended **KBC1 exports section** — a class list plus, per export, its consumer
name, its Kira name, the function it resolves to, and its parameter and result
types. The section is written only when there is something to export, so an
application's bytes are what they always were, and a module from before it
existed decodes as a module with no exports. Handles cross as
`BridgeValueTag::HANDLE` (tag 8, one opaque word owned by the side that minted
it), which the opposite direction — a Rust crate consumed *from* Kira — shares
rather than appending a second tag for.

### The VM engine: a Kira library as a Rust crate

`kirac build` in a library package produces two things — the artifact, and the
Rust crate a consumer actually depends on:

```sh
$ kirac build uifoundation.kira            # --backend vm is the default
Successfully built .kira-build/lib/uifoundation.kbc
  3 exports -> .kira-build/rust/uifoundation
```

```toml
[dependencies]
uifoundation = { path = "../uifoundation/.kira-build/rust/uifoundation" }
```

```rust
let ui = Uifoundation::load()?;
let button = ui.make_button("ok")?;     // a handle out
println!("{}", ui.button_label(&button)?);   // an owned String out
assert!(ui.click_at(&button, 4, 8)?);   // Rust re-entering Kira
drop(button);                           // releases the Kira object
```

The generated crate `include_bytes!`s the `.kbc` and runs it on a persistent VM
instance inside the consumer's process. **No linker, no LLVM, no `unsafe`** —
which is why this engine is the one provable on a machine that has none of them,
and why the crate also builds for `wasm32-unknown-unknown` — the Web answer
above.

A wrapper and the library it was generated from are built separately, so they can
disagree. The VM engine has no link step to fail, so the guard is data:
`load()` checks the embedded module's class list, every export's name, arity,
parameter types and result type, and finally a content hash — and names the first
thing that moved.

Exported classes become Rust newtypes over a handle. Dropping one releases the
object it names, and nothing else does; use-after-free is not expressible,
because every method borrows the handle and `Drop` consumes it. The wrapper types
are neither `Send` nor `Sync`: one instance belongs to one thread.

The generated crate is a build artifact — regenerated on every build, never
committed, its dependency paths true only of the checkout that produced it.

### The native engine: the same crate over compiled machine code

`--backend llvm` in a library package produces the other engine's version of the
same product — a static archive, and a Rust crate that links it:

```sh
$ kirac build --backend llvm uifoundation.kira
Successfully built .kira-build/lib/libuifoundation.a
  3 exports -> .kira-build/rust/uifoundation
```

**The consumer's code does not change.** `Uifoundation::load()`,
`ui.make_button("ok")`, `drop(button)` — the five lines above compile and run
against either engine, which is the property the whole feature is measured on.
What changes is entirely underneath.

The archive exports one **stable trampoline per export**, all sharing the uniform
C-ABI shape the hybrid seam already load-tests:

```c
void kira_lib_uifoundation_make_button(
    const BridgeValue *args, uint32_t count, BridgeValue *out);
```

One shape for every Kira signature, never a typed C symbol per export: a typed
symbol would re-open ABI drift — two separately compiled sides agreeing on a
struct-passing convention per signature, with nothing but a name to catch a
disagreement — to buy nothing the generated crate does not already hide. The
names are `kira_lib_<library>_<export>`, disjoint by prefix from the `kira_x_*`
namespace the opposite direction claims, so both can live in one process.

A handle is a **box**. Inside native code a class instance is a struct value that
dies with its frame, and a handle outlives the call by definition, so an export
returning one moves it into an allocation and hands back the address. Each
exported class gets a synthesized destructor, `kira_lib_<lib>_drop_<class>`,
which releases whatever the instance owned and then frees the box — and the Rust
newtype's `Drop` is the only thing that calls it. A handle *argument* is lent, so
the trampoline deep-copies before the call rather than letting the callee drop
the consumer's object.

The stale-build guard is a **symbol**, not data, because this engine has a link
step to fail: the library defines `kira_lib_<library>_abi_1` and the generated
`load()` calls it. An archive built under a different export contract does not
define it, so the consumer's **link** fails naming the marker — the exact lesson
`RUNTIME_ABI_VERSION` encodes, applied one level up. The marker's body also
references the runtime marker, so a library carries both guards; an executable
gets that one from its C `main`, and a library has no `main`.

The archive is self-contained: `llvm-ar` splices the Kira native runtime's
members in beside the library's object, so a consumer links one file and needs no
arrangement with the Kira toolchain. **The consumer's build needs no LLVM** —
LLVM compiled the library; linking an archive does not.

Three things differ from the VM engine, and each is a consequence of where the
code runs rather than a gap. `print` goes to stdout, with no host to redirect it,
because giving native code one would be an ABI rather than a parameter. A trap
ends the process, exactly as a `kira build` binary does — `attempt`/`try`/`handle`
inside the library is the portable way to keep one away from the boundary. And
two Kira native libraries cannot share a binary, for the reason above.

`--backend hybrid` is the third engine, and the only one that keeps the library
author's own `@Runtime`/`@Native` split meaningful. The other two ignore it — the
VM engine compiles everything to bytecode, the native engine compiles everything
to machine code — so hybrid is where a library's hot inner function is machine
code while its surface, its handles, and its strings stay on the VM.

A consumer enters through the **bytecode half**, always: a handle is a root into
that half's heap, and machine code cannot mint one. Two rules follow, both
refused at build time by function name — an `@Export` may not also be `@Native`,
and a `@Native` function may not call a `@Runtime` one, because a library
instance owns a heap and cannot be re-entered mid-call. An *application* built
with `--backend hybrid` may still call in both directions; a library gives that
up, and that is the whole of what it gives up.

It builds three artifacts. Two are data and are embedded in the generated crate
(`<name>.kbc`, `<name>.khm`); the third is a shared library the consumer's
process opens at load, so **deployment is one file**. It is looked for beside the
consumer's executable and then at the path the build recorded, or at
`KIRA_<LIBRARY>_NATIVE` if that is set — which overrides rather than leads the
search, so an operator who names a file gets that one or an error, never a
different one. A load that finds nothing names every path it tried. The costs are
real and stated in the generated crate's README: `libloading` enters the
consumer's dependency graph, and that crate does not build for
`wasm32-unknown-unknown` (the VM engine's does, and is the wasm answer).

See [.codex/work/kira-export-design.md](.codex/work/kira-export-design.md).

### The proof

`crates/kira-export-consumer` is a Rust crate whose `build.rs` compiles the Kira
library in `fixture/uifoundation/`, generates the wrapper through the same
generator `kirac` drives, compiles it under this workspace's lint gate, and calls
it.

[examples/library/](examples/library/) is the same thing written to be read
rather than run by CI: a small Kira library, and the Rust that calls it. Its
README's snippet was run against the generated crate before it was written down,
and `backend_parity/examples.rs` builds the package on all three engines, so a
claim there that stopped being true fails a test rather than going stale.

`tests/consumer.rs` is the parity proof, and its force comes from being one file
rather than three: it is compiled and run **unchanged** against every engine.

```sh
cargo test -p kira-export-consumer                            # VM engine
cargo test -p kira-export-consumer --features native-engine
cargo test -p kira-export-consumer --features hybrid-engine
```

A test that had to be edited to run against another engine would have disproved
the claim it was written to check. What an engine has that another does not is
stated rather than quietly weakening the shared file: `tests/vm_engine.rs` holds
what the two VM-family engines share (a custom host, live-handle accounting), and
`tests/hybrid_engine.rs` holds what only the hybrid engine has — a bytecode
export calling into machine code, and a native half that can be missing.

## Live sessions

`kirac live` builds a program into a `.klbundle`, serves it over a loopback
socket, and runs it on a runner client. `--watch` reloads on every save: a
bytecode-only edit swaps into the running process, and anything the process
cannot take in place relaunches it and says why.

```sh
kirac live app.kira                             # the VM half
kirac live --backend hybrid app.kira            # both halves
kirac live --watch app.kira                     # reload on every save
```

[docs/live.md](docs/live.md) covers the bundle format, the `live.*` event
vocabulary, the two reload tiers and how one is chosen, what is watched, and
what a hot patch does and does not preserve.

## Installing a toolchain

`knvm` provisions released toolchains into `~/.kira/toolchains` and selects
which one the `kira` launcher dispatches to:

```sh
knvm install latest        # install the newest release and select it
knvm binstall              # build this checkout and install it (dev channel)
knvm sinstall              # build knvm + kira themselves and put them on PATH
knvm list                  # what is installed; `*` marks the selected one
knvm use 1.7.3             # select an installed version
knvm uninstall 1.7.3       # remove one installed version
```

From a bare machine with this checkout: `cargo run -p kira-knvm -- sinstall`,
then `knvm binstall`, and `kira run program.kira` works.

[docs/knvm.md](docs/knvm.md) covers the tree an install produces, the install
pipeline and its atomicity, channels, `binstall`, `sinstall`, the offline
`KNVM_RELEASE_DIR` route, and `KIRA_HOME`.

## Editor support

`kira-lsp` builds `kira-language-server`, the language-server binary editors
talk to. Install it from a checkout of this repo:

```sh
cargo install --path crates/kira-lsp
```

It lands in `~/.cargo/bin`. The server speaks LSP over stdio, takes no CLI
arguments, and serves **diagnostics only** — it handles `initialize`,
`didOpen`, `didChange`, `didClose`, and publishes diagnostics. It advertises
full-document sync and nothing else, so a client knows not to ask for more;
anything that asks anyway gets `MethodNotFound` rather than a wrong answer.
Hover, completion, and goto-definition are not implemented yet.

Analysis is **per-document, whole-program**: each open document is analyzed as
the *entry* file of a program, and every module it imports is read off disk and
analyzed with it. So a name a sibling module declares resolves in the editor,
and an `import` that names no file squiggles.

Two consequences worth knowing. Modules are read **from disk**, not from open
editor buffers, so an unsaved edit in `support.kira` is not what `main.kira` is
checked against. And diagnostics are still published for one document at a
time: an error inside an imported module is reported when you open *that*
file, not as a squiggle on the file that imported it. Nothing is reported for a
file that is not open.

The server runs the same salsa frontend `kirac check` does, so an editor
squiggle and a command-line error are the same computation rather than two
implementations that agree until they do not.

### Zed

The [Kira Zed extension](https://github.com/kira-lang-com/kira-zed-extension)
provides syntax highlighting via Tree-sitter plus diagnostics from the server
above. Install the server first — the extension does not bundle it and, since
`kira-lsp` is unpublished, cannot download it. The extension finds the binary
on the worktree's PATH; to point at a specific build instead, set an explicit
path in Zed's `settings.json`:

```jsonc
{
  "lsp": {
    "kira-lsp": {
      "binary": { "path": "/absolute/path/to/kira-language-server" }
    }
  }
}
```

Restart Zed after installing or replacing the binary.
