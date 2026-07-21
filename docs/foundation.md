# Foundation

`import Foundation` resolves against a package installed beside the compiler
rather than beside the program. Nothing else in the language reaches outside a
project's own directory, and this is the mechanism that lets a standard library
exist at all.

Foundation declares `printLine` and a small geometry and math vocabulary — the
subset of the reference implementation's `Types/Maths` the migrated corpus
constructs and reads:

```kira
import Foundation

@Main
function main() {
    printLine("hello")
    let frame = Rect(x: 0.0, y: 0.0, width: 640.0, height: 480.0)
    let origin = Point { x: frame.x, y: frame.y }
    let extent = Size(frame.width, frame.height)
    return
}
```

## The geometry vocabulary

`Point`, `Size`, `Rect`, `Vec3`, and `Mat4` are data structs, matched to the
reference implementation on field names and types so a construction written
against it means the same thing here. `Mat4` defaults to the identity; the free
function `mat4Identity()` returns it. `sqrtApprox`, `sinApprox`, and `cosApprox`
are the series approximations the corpus's own math builds on.

A data struct is constructed by naming it, its implicit memberwise constructor:
`Point(1.0, 2.0)` fills the fields in declaration order and `Point(x: 1.0, y:
2.0)` binds each by name, the two spellings the `Point { x: .., y: .. }` literal
already had. A field the call does not reach takes its declared default, so
`Mat4()` is the all-defaulted value. The constructor lowers to the same struct
value a literal produces, so it runs identically on every backend.

## Where the compiler looks

An installed toolchain is `<root>/bin/kirac` and `<root>/foundation/`, so
discovery anchors on the running executable and resolves its directory's
sibling. That is the primary rule, and it consults neither `$HOME`, nor
`current.toml`, nor the working directory: move a toolchain and its standard
library moves with it, still matching the compiler it was installed with. A
version-skewed pairing of one toolchain's `kirac` with another's Foundation is
unreachable.

Three rules surround it, in `kira-toolchain`'s `bundled_discovery`. Setting
`KIRA_FOUNDATION_HOME` wins outright and never falls through — a wrong override
is an error naming the path, not a silent fallback, the same contract
`KIRA_LLVM_HOME` carries. Failing that, the toolchain named by
`~/.kira/toolchains/current.toml` is tried, which is the route for a consumer
that is not `kirac` itself: a `build.rs` compiling a Kira library through
`kira-build` runs as a Cargo build script sitting nowhere near a toolchain.
Last, and only after both have failed, the walk looks upward from the executable
for a directory holding both a workspace `Cargo.toml` and `foundation/package.kira`
— which is how a `kirac` built into this repository's `target/debug/` finds the
`foundation/` committed here. A shipped toolchain never reaches that rule, so
the shipped path never depends on a checkout existing.

Both markers are required for the checkout rule. A workspace `Cargo.toml` alone
would match any Rust project the compiler happened to be built inside, and a
bare `foundation/` alone would match a user directory that happens to be called
that.

## What the import means

Foundation is a package, not a prelude. A file that does not write
`import Foundation` cannot call `printLine`, and reports `KSEM061` for trying —
matching the reference implementation, whose corpus re-imports Foundation in
every file that uses it. Beyond that the import behaves like any other: it binds
`Foundation` as a namespace root in the file that wrote it, so both `printLine`
and `Foundation.printLine` resolve there, and a sibling file that wants the root
writes its own import or hears `KSEM027`.

A bundled package answers only the namespace its manifest's `moduleRoot`
declares. Foundation can resolve `Foundation` and `Foundation.Web`; it can never
resolve `support`. A toolchain able to satisfy any import would make every
program's meaning depend on what happened to be installed.

Below that gate nothing is special-cased. A module path is a path under the
package's `app/` directory with dots as separators, exactly as it is under a
program's own directory, so `Foundation` is `app/Foundation.kira` and
`Foundation.Web` would be `app/Foundation/Web.kira`.

## Collisions

The project always wins. A `Foundation.kira` written beside a program is the
module that loads, and the bundle is consulted only when the program's own
directory has no such file. Installing a new toolchain cannot change what a
program that shipped its own module by that name means.

A program that declares a name Foundation also declares gets `KSEM003`, the
ordinary duplicate-declaration error, where the second declaration is written.
An imported package's declarations are the program's declarations; there is no
separate rule for a bundled one.

## What Foundation does not have yet

The geometry structs are pure data here. The reference implementation also gives
`Vec3`, `Mat4`, and `Quaternion` their vector and matrix algebra as methods —
`add`, `dot`, `cross`, `normalize`, `multiply`, `translate`, and the rest — and
the corpus does not call any of them (its own math is written out longhand). The
matrix methods lean on struct operator overloading (`self * Mat4 { … }`), which
this compiler has not built, so they wait on it rather than being ported in a
form that would not compile. `Vec2`, `Vec4`, and `Quaternion` themselves are
unported for the same reason: nothing in the corpus constructs them.

The reference implementation's Foundation also carries seven more files. Each
waits on a language subsystem this compiler has not built, and each is named
here rather than stubbed — a file that compiles and does nothing is worse than
an import that fails.

| File | Waits on |
|---|---|
| `Result.kira` | nothing; generic enums landed, and it is the next file to port |
| `Printable.kira` | `construct` declarations |
| `Test.kira` | `comptime` and `construct` |
| `Derive.kira` | `comptime macro` |
| `DeriveSerde.kira` | `comptime macro` |
| `FileSystem.kira` | FFI, and the native-library build that binds `fs.c` |
| `Web.kira` | FFI, and the DOM bindings |

Foundation is the consumer of nearly every remaining subsystem, which is why it
is worth having the mechanism before the content: each of those files becomes a
matter of adding a `.kira` file to `foundation/app/`, with no change to how an
import finds it.
