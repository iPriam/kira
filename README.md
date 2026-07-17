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
