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
function `mat4Identity()` returns it. The trigonometry and roots the corpus's
own math builds on are compiler primitives — `sqrt`, `sin`, `cos`, `tan`,
`floor`, `ceil`, `abs` — not library functions, and `docs/language.md` covers
them.

A data struct is constructed by naming it, its implicit memberwise constructor:
`Point(1.0, 2.0)` fills the fields in declaration order and `Point(x: 1.0, y:
2.0)` binds each by name, the two spellings the `Point { x: .., y: .. }` literal
already had. A field the call does not reach takes its declared default, so
`Mat4()` is the all-defaulted value. The constructor lowers to the same struct
value a literal produces, so it runs identically on every backend.

## The filesystem

`FileSystem.kira` reads and writes files on every backend. `readFile` and
`writeFile` work in text, `readFileRange` and `writeBytesFile` in bytes,
`listDirectory` walks a directory, and `fileExists`, `pathExists`,
`isDirectory`, `fileSize`, `makeDirectory`, `renamePath`, and `removePath`
answer questions about a path.

Nothing here fails. A missing file, a parent directory that does not exist, a
directory where a file was wanted — each answers `false`, `0`, or an empty
array, because a program has to be able to ask the outside world a question and
hear no. Two answers are worth knowing before relying on them: `removePath` is
recursive on a directory, and `readFile`'s `text` stops at the first NUL byte
while its `size` counts the whole file, so binary data belongs in
`readFileRange`. Both match the reference implementation, measured by
differential run rather than assumed.

Underneath, these sit on compiler intrinsics rather than on a bundled C library.
The VM describes each operation and hands it to its host through
`HostCapabilities`; native code calls `kira_rt_fs_*`. Both reach one
implementation in `kira-runtime-abi`, which is what makes the three backends
agree byte for byte, and it is why the portable VM core still builds for
`wasm32-unknown-unknown` — a host that grants no filesystem simply refuses, and
the web has no files to grant.

## Images

`Image/` decodes PNG and JPEG into RGBA8:

```kira
match decodeImageFile("assets/backdrop.png") {
    Ok(image) -> { printLine(String(image.width) + "x" + String(image.height)) }
    Error(reason) -> { printLine("no picture there") }
}
```

`Image` is `width`, `height`, and `pixels` — four bytes per pixel, row-major,
**first row at the top**, no padding between rows. `decodeImage` takes the bytes
directly for a caller that already has them, and names the format from the bytes
rather than from a path's extension.

What is read: every PNG colour type and bit depth the format defines, palettes,
both kinds of transparency, and Adam7 interlacing; and JPEG's sequential Huffman
frames — `SOF0` and `SOF1`, greyscale or three-component, at any sampling
factors and any restart interval, which is what a camera or an exporter writes.

`ImageFailure` names three answers. `FileUnreadable` is a path with nothing
behind it. `UnknownFormat` is bytes carrying an encoding this does not read — a
GIF, but equally a progressive or arithmetic-coded JPEG, which are different
encodings sharing a container. `Malformed` is reserved for bytes that claim one
of the encodings here and then contradict it, so a caller retrying a download
can tell damage from a file it was never going to read.

Checksums are not verified — neither a PNG chunk's CRC nor a zlib stream's
Adler-32. A checksum answers whether bytes travelled intact, which is a
different question from whether they decode, and refusing a file that decodes
would throw away a picture the caller can use. Structure is checked, because
nothing can be decoded past a length that overruns its input.

`Compression/` holds what PNG needs and nothing else needs to duplicate:
`inflateZlib` and `inflateRaw` read DEFLATE (RFC 1951) and its zlib wrapper
(RFC 1950), answering `InflateFailure.Truncated` or `.Malformed`. A compressed
stream is not a graphics concept, which is why it is here rather than inside the
decoder that first wanted one.

All of it is Kira. There is no bundled decoder and no C to link, so a program
reads a PNG on the VM, on native code, and anywhere else the language runs.

## The compiler

`Kira/Compiler.kira` lets a Kira program compile Kira. The unit is a **package
set**, not a source string: `checkPackages` takes a `KiraCheckRequest` — a list
of `KiraPackage`, each a `package.kira` text plus its named source files, with
one of them named as the root — and answers with a `KiraCheckResult` holding
typed `KiraCheckDiagnostic` values.

```kira
import Foundation

@Main
function main() {
    var package = KiraPackage()
    package.manifest = "Package App {\n    let kind = .App\n}\n"
    var file = KiraSourceFile()
    file.path = "app/main.kira"
    file.text = "@Main function main() { print(missing) return }"
    package.files.append(file)

    let result = checkPackage("App", package)
    print(result.ok())                                 // false
    print(result.has(.KSEM060, "app/main.kira"))       // true
    return
}
```

A package set rather than a string, because the bugs worth catching are
multi-file. An `import` is written per file and binds that file only, so
`import Foundation` in `app/A.kira` is invisible in `app/B.kira`. A package is
one flat namespace, so two of its files declaring one name collide. A library
plus the app on top of it is two packages with an edge between them. None of
that can be said with one string of source.

Each diagnostic carries the code as a `KiraError` value, its severity, and the
**file** it points into — so a test asserts `.KSEM061` in `app/FileC.kira`
rather than matching text that gets reworded. `codeText` carries the code
exactly as the compiler wrote it, for a code this Foundation's enum does not
list; `kiraErrorFromCode` in `Kira/DiagnosticCodes.kira` is the mapping, and it
answers `.Unrecognized` for one it does not know.

Nothing reaches a disk. The files checked are the ones written in the request
and no others, so two checks cannot see each other's work and nothing is left
behind — the one exception being a bundled package an `import` names, which is
Foundation itself. Checking runs the frontend and nothing after it: no IR, no
code generation, no linker, so a suite of these needs no toolchain installed.

Underneath, this is a host capability like the filesystem. The VM describes the
request and hands it to its host; native code calls
`kira_rt_compiler_check_packages`. Both reach one `kira-check` session, which is
what makes the three backends agree. A host with no compiler — an embedded VM, a
browser tab, a test — refuses **by name** rather than answering "no
diagnostics", which would read as "it compiled". `kira` is the host that grants
it, because `kira` is what links the frontend.

A native build links the compiler only when the program reaches it: `kira`
links `libkira_compiler_bridge.a` for a program that calls `kcCheckPackages` and
the small `libkira_native_bridge.a` for every other, so no Kira program carries
a compiler it never calls.

## Where the compiler looks

An installed toolchain is `<root>/bin/kira` and `<root>/foundation/`, so
discovery anchors on the running executable and resolves its directory's
sibling. That is the primary rule, and it consults neither `$HOME`, nor
`current.toml`, nor the working directory: move a toolchain and its standard
library moves with it, still matching the compiler it was installed with. A
version-skewed pairing of one toolchain's `kira` with another's Foundation is
unreachable.

Three rules surround it, in `kira-toolchain`'s `bundled_discovery`. Setting
`KIRA_FOUNDATION_HOME` wins outright and never falls through — a wrong override
is an error naming the path, not a silent fallback, the same contract
`KIRA_LLVM_HOME` carries. Failing that, the toolchain named by
`~/.kira/toolchains/current.toml` is tried, which is the route for a consumer
that is not `kira` itself: a `build.rs` compiling a Kira library through
`kira-build` runs as a Cargo build script sitting nowhere near a toolchain.
Last, and only after both have failed, the walk looks upward from the executable
for a directory holding both a workspace `Cargo.toml` and `foundation/package.kira`
— which is how a `kira` built into this repository's `target/debug/` finds the
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

Below that gate nothing is special-cased. `import Foundation` names the package,
so it loads every `.kira` file under `app/` into one flat scope — the same thing
a dependency import by bare name does, and what lets Foundation hold
`Foundation.kira` and `FileSystem.kira` without either being reachable only by a
spelling nobody writes. A dotted import is a path instead: `Foundation.Web`
loads `app/Foundation/Web.kira` and nothing else.

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

`Derive.kira` and `DeriveSerde.kira` ship the four builtin derive macros —
`Equatable`, `Clone`, `Serializable`, and `Deserializable` — written in pure
Kira on top of `comptime macro`. `docs/macros.md` documents what each generates,
the field classification they share, and the wire format the serde pair reads
and writes.

`Result.kira` ships `enum Result<Value, Failure>`. An importing program writes
`Result<Int, Trouble>` in any type position and constructs it with either
spelling — `.Ok(1)` or `Result.Ok(1)` — because a qualified constructor carries
no type arguments and takes its instantiation from the position it fills. A
qualified spelling with nothing to anchor it is `KSEM254` rather than a guess.

The reference implementation's Foundation carries one more file, `Web.kira`,
which waits on FFI and the DOM bindings. It is named here rather than stubbed —
a file that compiles and does nothing is worse than an import that fails.

Foundation is the consumer of nearly every remaining subsystem, which is why it
is worth having the mechanism before the content: that file becomes a matter of
adding a `.kira` file to `foundation/app/`, with no change to how an import
finds it.

## The test vocabulary

`Test.kira` ships the `Test` construct family a suite is written in, together
with `TestResult`, `TestReport`, `TestStatus`, `TestFailure`, and `TestRuntime`.
A case is a declaration backed by `Test` providing the two members the family
requires:

```kira
Test SumsToTen {
    test { return add(4, 6) }
    expect { let e: Result<Int, TestFailure> = .Ok(10); return e }
}
```

Nothing in the compiler knows the name `Test`. It is one construct family among
any others a library could declare, and every rule that shapes a case is a rule
of the construct surface — which is why `Test.kira` is a `.kira` file and not a
branch in the frontend.

Both requirements write a result type — `test() -> Any` and
`expect() -> Result<Any, TestFailure>` — because `Any`, Kira's top type, names
exactly what a case answers with: whatever it measures. A `test { … }` member
therefore returns `Any` and an `expect { … }` member a `Result<Any,
TestFailure>`, which is what a runner reads without the family knowing what any
one case measures.

A case answers with the narrow instantiation it measured — `Result<Int,
TestFailure>` above — and a `Result<Any, TestFailure>` position accepts it. That
is the one widening the language has beyond `Any` itself: an instantiation
reaches another instantiation of the same template when every type argument
either stays as it was or becomes `Any`. Nothing about it knows the name
`Result`, so a user's own `enum Crate<Held>` widens by the identical path. The
boundary is that the arguments widen and a position that merely *contains* one
does not: `[Result<Int, TestFailure>]` is not `[Result<Any, TestFailure>]`, for
the same reason `[Int]` is not `[Any]`.

The result type comes from the family, not from the shorthand. A `name { … }`
member is the body of the member the family calls `name`, so what it returns is
the family's to state; a family that declares no such member falls back to the
family type, which is what keeps a `body { … }` on a family that never mentions
`body` meaning what it always did. No name is special-cased — `test` and `expect`
take the same path a user's own family and member would.

`Any` is one-directional. A value crosses in and is stored, copied, passed,
returned, and released, and nothing reads it back: the language has no `is`,
`as`, or downcast form, so a runner still reaches a case's own result through its
declaration rather than through an `Any Test` value.

`Printable.kira` ships the `Printable` family, whose one requirement is
`onPrint() -> String`. It is the whole surface: `print(value)` does not consult
it, and there is no `@Printable` annotation here, so `onPrint()` is called by
name like any other member.
