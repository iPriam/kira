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
| 4 | `kira-glsl-backend`, `kira-wgsl-backend`, `kira-hlsl-backend`, `kira-msl-backend`, `kira-spirv-backend`, `kira-bytecode`, `kira-vm-runtime`, `kira-native-bridge`, `kira-hybrid-runtime`, `kira-debug`, `kira-llvm-backend`, `kira-wasm-runtime` |
| 5 | `kira-manifest`, `kira-project`, `kira-package-manager`, `kira-build-definition` |
| 6 | `kira-program-graph` |
| 7 | `kira-build` |
| 8 | `kira-instruments`, `kira-linter`, `kira-doc`, `kira-app-generation`, `kira-live` |
| 9 | `kira-cli` (binary `kirac`) |
| 10 | `kira-main` (C ABI facade: staticlib/cdylib/rlib) |
| runners | `kira-desktop-runner` (binary `kira-desktop-runner`) |
| tools | `kira-bootstrapper` (binary `kira`), `kira-devflow` (binary `devflow`) |

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

The one operation on an enum is comparison: `==` and `!=` compare
**discriminants**, so `c == .Red` asks which variant `c` is. Reading a payload
back out is `match`, a separate feature — so a payload is built, moved, and
dropped, but not yet inspected.

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
- **A payload may be `Int`, `Float`, `Bool`, or `String` only.** A
  struct/enum/array payload is refused (`KSEM118`): the runtime box carries one
  type-erased word, which an aggregate has no form in yet.

See [.codex/work/enums.md](.codex/work/enums.md) for the design, and
[examples/enums/enums.kira](examples/enums/enums.kira) for a tour.

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

Analysis is **per-file**: each open document is analyzed alone, because the
language has no imports or modules yet. There are no project-wide diagnostics,
and nothing is reported for a file that is not open.

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
