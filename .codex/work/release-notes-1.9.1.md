# Kira 1.9.1

Kira 1.9.1 adopts the 1.9.1 language semantics across the toolchain. A program
can ask what a value *is* and answer a failed cast; it can name a distinct type,
declare a constant at module scope, hand work to the host's main thread, and pass
values between its own contexts over a channel. The FFI seam learns Bool, a
written null pointer, and a C header's enumerators. The Linux system-call table
grows from fourteen entries to fifty-one. Native builds can be instrumented
behind `--sanitize`, diagnostics are generated from one table with a drift gate,
and lints have levels and groups and can finally see the whole of a program.

**Attribute bodies are comma-separated now.** See below before upgrading.

---

## Breaking changes

### Attribute members take commas, and no trailing separator

    @FFI.Extern { library: ffifixture; symbol: ffi_add_i32; abi: c; }   // 1.8.x
    @FFI.Extern { library: ffifixture, symbol: ffi_add_i32, abi: c }    // 1.9.1

The trailing `;` on a bodyless declaration goes with it. Every attribute is
affected — `@FFI.Extern`, `@FFI.Syscall`, `@FFI.Struct`, `@Derive`, and the rest
— so a package written against 1.8.x will not parse until it is converted.

### `@Derive(Deserializable)` is a deprecated alias

`@Derive(Serializable)` now generates `serialize_T` and `deserialize_T` together.
`Deserializable` remains for the reader alone and should be removed from new
code.

### The Serde wire text changed

The wire is canonical now — see *Foundation* below. Text written by 1.8.x does
not read back under 1.9.1, and the parity programs that pinned the
pre-canonical spelling pin the canonical one.

---

## The host main thread

Some work must happen on the process's main thread — a window, a view, anything
a platform UI framework will only accept there. 1.9.1 makes that a surface
rather than a convention.

    let view  = MainThread.invoke { makeView("Kira") }   // runs there, waits
    let later = MainThread.spawn  { makeView("later") }  // runs there, joinable
    MainThread.post { makeView("notification") }         // runs there, no result
    print(later.await)

`@MainThreadLifecycle` is a preserved event loop that shares the process main
thread with other lifecycles and with `@MainThread` work. It is started by
calling it from `@Main`; the call queues a new instance and returns `Void`, and
the instance keeps its locals and its call stack between cooperative slices. **A
stalled application thread cannot stop an already-started lifecycle.**

A lifecycle is one execution however many slices it runs in, so the channels and
tasks it made are still its own after every suspension: an end taken before a
slice boundary names the same channel after it. Pinning is transitive — a
function reached only from main-thread work is itself pinned. The Web target has
no second thread to be pinned to, and refuses the whole family with `KSEM338`.

## Runtime type identity

### `value.type`

1.9.1 requires a runtime type descriptor, and every rule it states about one is a
rule about identity: **equality is exact package-qualified nominal identity, a
`distinct` is itself rather than its representation, and two packages'
same-named declarations are two answers.**

`value.type` reads the same identity `Any`, `is` and `as` already carry.
`ErasedTypeId` becomes a family word plus a row in a per-program descriptor
table, built during lowering while a `distinct` is still itself, and the tag an
erasure writes is fixed there rather than re-derived by each backend from the
payload's machine form. **One table serves erasure, the type test, the downcast
and the descriptor**, so none of them can drift from the others.

A descriptor exposes a name, a package, a kind, the arguments an instantiation
was minted with, and the traits the type keeps. It exposes nothing else: fields,
layout, methods and source spans are compile-time facts, and a runtime descriptor
carrying them would make every declaration's private shape public. Reading
anything else is `KSEM363`; asking `.type` of an expression that names no value
is `KSEM362`.

### A failed cast a handler can answer

`try value as Type` is the cast as a fallible step. It yields the `Result`-shaped
value the `attempt` machinery already consumes — `Ok(target)` or
`Error(TypeCastError.Mismatch(Type))` — so exhaustiveness, failure agreement and
handler resolution are the rules every other failure obeys rather than new ones.
**A cast written without `try` still traps.**

The failure point stays written, because Kira has no unwinding. `try` is accepted
in exactly one position so that every early exit from a body is visible in the
body. It binds looser than `is` and `as`, which makes `try value as T` the cast
being tried rather than a cast of what was tried — the only reading with a
meaning. `Mismatch` carries the descriptor the value actually held, which is the
one fact a handler cannot recover for itself.

### Distinct types

`distinct Name = Representation` mints a nominal type over an existing one: same
layout, different identity, and not interchangeable with what it wraps. It is
what channel ends are built from, and `DnxDistinctTests` covers it across all
three engines.

## Module-scope constants

`let name = value` at module scope, ordered by resolved references rather than by
the order they were written, so a constant may name one declared below it. A
program past the module limit is now **refused** rather than having its tail
silently dropped.

## Channels

`Channel<T>()` creates a channel and hands back its **sender**. The matching
**receiver** is read off it as `.receiver` — a derivation rather than a second
creation, so there is one channel however many times it is read.

    async function fill(tx: Sender<Int>) -> Int {
        tx.send(41)
        tx.send(1)
        tx.close()
        return 7
    }

Ends are minted `distinct` rows over `Int`. That buys nominal identity and scalar
layout, and the layout is the requirement rather than a convenience: an end is
moved into the task that uses it, and a task argument slot is one word.

The wait is synthesized IR, one function per payload, so both engines run the
same wait rather than two that can drift. A drained closed channel is a typed
failure through `attempt`; sending to a channel whose receiver is gone is a trap.

**A receive on an empty channel with nothing runnable used to hang forever.** It
raises `ChannelTrap::Deadlock` now — deliberately not `Closed`, which would claim
the sender went away when it did not.

**Heap payloads** cross on the existing native-state transport: send boxes the
value and queues the token, receive materialises and releases. No new runtime
symbol and no ABI bump, because a token is one word and the channel primitive
contract already carries words. `native_state_type_id` derives from the type's
shape, so the sender's box and the receiver's recovery agree by construction
rather than by protocol. Closing a receiver drains and releases what is still
queued, and a run that ends with payloads never received hands them back rather
than leaking them.

`KSEM365` refuses a pointer payload because an address means nothing in the
receiving context, and it asks that of **the whole value** — across structs,
arrays, enum payloads, distincts and cells — naming the pointer it found.

A hybrid program keeps **one** channel table. A `@Runtime` `main` creating a
channel and passing the sender to a `@Native` function used to trap with `channel
end handle is not live`, because the two halves kept separate tables. The
bytecode half performs channel primitives on the native half's table through
`HostCapabilities::channel_op`, which returns `None` by default so every other
host keeps its own. The archive gains `kira_rt_channel_try`, which answers a trap
code rather than exiting, because the VM raises its own trap.

## System calls

The table carried fourteen system calls, which is enough for a program that
reads, writes and exits, and not enough for one that supervises processes, polls
descriptors and talks to a device. **It carries fifty-one now.**

Three of them decide the shape of the rest:

- **`clone3` rather than `clone`.** The legacy call takes its arguments
  positionally and the last two are in a different order on aarch64 than on
  x86-64, so one table would have to encode that difference per architecture.
  `clone3` takes a struct and has one argument order everywhere.
- **`waitid` rather than `wait4`**, because it takes a pidfd.
- **`pidfd_send_signal`**, because a pid is a name that can be reused between the
  moment it is read and the moment it is signalled, and a descriptor is not.

Each entry records whether an interpreter may serve it under `--backend vm`. The
rule is whether the call is bounded by a descriptor the program already holds, so
`mmap` (which would map into the interpreter's address space), `clone` (which
would fork the interpreter) and `getpid` (which would answer with its identity)
are refused there by name. Seven calls are refused this way, and an interpreted
declaration's signature is validated at the host seam rather than trusted.

## The FFI seam

**Bool crosses the C seam**, and the fix is not cosmetic. On LLVM 23.1.0, `llc
-O2` on `load i1` emits a bare `ldrb` with no normalization, so a `_Bool` byte of
2 becomes an "i1" holding 2. The `i8` + `icmp ne` form emits `ldrb` + `cmp`. The
parity fixture presents a deliberately non-canonical `_Bool` byte of 2 and
confirms it survives both by value and behind a pointer. The Bool byte and the
null pointer reach the wasm seam too.

**`RawPtr.null`** is a member a program writes and compares, rather than a cast
from zero, and the raw-pointer module carries pointer equality. Foreign
declarations the seam cannot honor are refused at analysis time.

**A foreign result is read as its own size**, not as the word libffi rounded it
up to. **A plain `char`'s signedness comes from the ABI**, not from the
architecture: `plain_char_spelling()` decided from architecture alone, but Apple
and Microsoft override `char` to signed on every architecture they ship.

**Float→U64** gained `ConvertFloatToUInt`, the unsigned twin of a conversion that
already existed on the other side. Both engines previously trapped on `1e19`,
which fits `U64`.

Autobind profiles are removed and native-library diagnostics say what they could
not find.

### Autobinding carries a header's enumerators

A binding module held structs, functions, arrays, pointers and callbacks, and had
nowhere to put a number. So a package generating its types from a header still
wrote that header's constants out by hand — `DRM_IOCTL_MODE_SETCRTC` and every
value like it was a hexadecimal literal in Kira source that nothing checked
against the header it came from.

Enumerators now come across as module-scope `let`s, under the same rule the rest
of the seam follows: every one the listed headers define under `AllPublic`, or
the named ones under a `constants` selection. The value is what clang evaluated
for the target being built, so an enumerator whose width or sign depends on the
platform is read rather than assumed.

The enum type itself is deliberately **not** declared. Kira's enum is a tagged
union with payloads and exhaustive matching; C's is a set of integers in a shared
namespace that callers freely combine with `|`. Binding one as the other would
promise a totality C never had, so the values cross and the type does not.
Selection is therefore by enumerator, never by the enum that holds it — which is
also the only thing that works for the anonymous enums headers use to write bare
constants.

A `#define` still cannot be bound, and now says so: the preprocessor has consumed
it before there is a cursor to walk, so a declaration naming one gets the same
recorded reason as any other declaration the seam cannot carry.

## Traits, generics and dispatch

- **Traits on every value type**, with static dispatch, written defaults,
  supertraits discharged as requirements, and retroactive conformance.
- **Trait existentials**: a trait's name in a type position is an existential
  over its conformers, reading the shapes the trait requires.
- **Generic type semantics complete**, including generic enums, `Any` erasure,
  and a parameter's trait bounds discharged at every instantiation.
- **Mutable dispatch**: a mutating method's receiver is preserved through
  conformance rather than losing its mutability at the trait boundary.
- `Send` and `Sync` are compiler-known markers.

### Class dispatch is one dispatch

Both class harness files carried a parity rule: no inherited base method may call
`self.<m>()` where a descendant overrides `<m>`, because that dispatch was virtual
on vm and hybrid and static on llvm. Both steered around the shape entirely, so
**the language's most obvious inheritance behaviour was the one thing untested.**

It does not diverge. Classes are monomorphized: a parent's body is registered once
per descendant with the receiver typed as the concrete class, so `self` is
statically the leaf, and static and virtual dispatch are the same dispatch on
every backend. Measured on vm, llvm and hybrid across seven cases — an inherited
body reaching a descendant's override, two hops of it, an override calling up to a
grandparent whose body dispatches back down through `self`, the same across two
parents, and an instance reached through an array rather than a binding.

## The mid IR

Expressions lower through a mid IR now, and `scope_releases` walks each body to
decide *when* ownership ends rather than only what owns what. `Drop` bodies run
once on every engine, and a `Drop` value is refused at every position that cannot
release it exactly once. Type erasure moves into `erase.rs` against the descriptor
table above.

## Foundation

### JSON

`foundation/app/Json/` is the tree and its total accessors (`Value.kira`), the
reader (`Parse.kira`), decimal conversion kept apart because the arithmetic is
where a number parser is right or wrong (`Number.kira`), and both writers
(`Write.kira`).

Accessors are total and refusals carry a byte offset, because JSON arrives from
elsewhere and a remote schema change should not become a crash. Nesting is
bounded; the reader recurses on untrusted input.

The numeric edges, each fixed as the case that was wrong:

- **A scale that counted digits it had not used.** Once the mantissa filled,
  further digits were discarded, but only the ones left of the decimal point
  adjusted the scale — so the number was divided by a digit it was never
  multiplied by. `0.123456789012345678901` read as roughly
  `0.0012345678901234567`, a hundredfold error rather than the nearest `Float`.
- **An integer range that stopped one short.** `Int`'s minimum has a magnitude one
  larger than its maximum, so measuring a literal before applying its sign refused
  `-9223372036854775808`, which fits. The accumulator runs downwards now and the
  cutoff is 2^63 exactly.
- **An exponent the peer chooses.** Scaling stepped through it 22 multiplications
  at a time, so `1e1000000` cost about a million of them to reach an infinity it
  was always going to reach. Clamped to the range a `Float` can still notice.
- **NaN.** `jsonInt` refuses what it cannot convert by comparing against `Int`'s
  bounds, and every comparison against NaN is false, so it passed both checks and
  reached a conversion that traps. It answers the documented fallback now.
- **`readKeyword` blamed the wrong byte.** `trxe` reported the offset of the `t`;
  it refuses at the byte that broke the keyword, one byte at a time.
- **`writeJson` wrote a whole `Number` as `3`**, and `3` reads back as an
  `Integer` — the distinction those two variants exist to keep. A trailing `.0`
  costs two bytes and preserves it.

The documentation no longer claims a round trip for infinities and NaN, which are
written `null` and read back as `Null`.

### The canonical Serde wire

`@Derive(Serializable)` generates the reader and the writer together, and the wire
is canonical:

- integers are tagged with their width,
- a float crosses as the bits it is, rather than as a rendering,
- strings are escaped,
- fields carry the canonical separators,
- arrays serialize nested to any depth.

**The reader takes the full input**: trailing text, wrong order, and duplicate or
unknown labels all trap. Qualified element spellings are refused with the element
rule rather than `KMAC013`. The deserializer body lives once in a shared function
both macros call, and the comptime type helpers move to `SerdeText` beside the
runtime ones.

### Derives and values

- **`Ordered`**, with its suite restored.
- **`Hashable`** and **`Tagged`**.
- **Geometry and struct operators complete**: struct arithmetic desugars to
  `add`, `subtract`, `multiply` and `divide` methods.
- **The floating-point primitive surface**: `sqrt`, trigonometry, rounding,
  logarithms, powers and binary math, covered on every engine.
- A member read from a temporary string lowers correctly.

## Lints

**A lint could not see most of a Kira program.** A lint walks statements, and
`statements_of` answered for a function and returned nothing for every other form.
The kik harness is written as constructs, so every structural lint had run over
the corpus the compiler is judged against and reported nothing, for as long as the
corpus has existed.

Three layers were blind, not one: `statements_of` read only a function's body;
`procedural::top_level` never scanned `trait` or `extend`, so neither reached a
macro at all; and `DeclarationKind` had no variant for either, so anything that
did arrive was dropped as `Other`. A body is now read from a function, a struct's
methods, a class's methods, a construct's members and initializers, an `extend`
block, and a trait's written defaults. `extend` is matched by text, because it is
a contextual keyword with no token of its own.

That found **107 manual index loops where it had found none**. 105 are rewritten by
`kira lint --fix`, which applies cleanly under nesting; the harness passes
identically on vm and llvm at 1544 cases before and after. The two left are tests
*about* `while`.

### Levels

`enabled: Bool` beside `severity: String` made `enabled = false, severity =
"error"` a state that parses, reads as emphatic, and means nothing. **`LintLevel`
is `Allow | Warn | Deny`**, which is what `kira-linter`'s own Rust enum has always
been — only the surface a package writes disagreed.

Every lint has a level before any package says anything, so `kira lint` in a
package that configured nothing runs the standard set. **It used to run nothing
and print `ok`**, which is the shape of a fake success. Entries in a `linter.kira`
are matched by their `code` rather than by the declaration's name — matching on
the name made it load-bearing, so renaming an entry silently turned its lint off.

### Groups

A level says what a finding costs; a group says whether the lint is asked at all.
Conflating them meant `--lint-level=deny` could shout about what already ran and
could never turn on what did not.

**`LintGroup` is `Correctness | Style | Complexity | Perf | Pedantic |
Restriction`.** Everything but the last two runs by default. A package asks for
more with `let groups: [LintGroup] = [.Pedantic]`. That reclassifies the
file-length ceiling as `Restriction` — a policy a project chooses — and the
constant-function lint as `Pedantic`.

### The command line, and a single site

`--lint-level=<level>` moves every code; `--allow`, `--warn` and `--deny` each
move one, so CI can deny what a package warns without editing the package. It
escalates what was reported and cannot resurrect a lint the package allowed.
Without a flag the policy passes everything through unchanged: a default of `Warn`
applied over the runner's own decisions would quietly demote every denied lint.

`@Allow(KLINT002)` above a declaration turns one lint off for it alone.
Annotations reach a macro for the first time here, as the list of what was written
rather than as the declaration's text — searching the text would find
`@Allow(KLINT002)` inside a string in the body just as readily.

### KLINT004

A zero-argument function whose whole body returns a literal.
`function limit() -> Int { return 700 }` is a constant spelled as a call, and Kira
has module-scope `let`, so it can be one. Narrow on purpose: zero parameters,
exactly one statement, that statement a `return`, and the returned text a literal.
It covers integers, floats, negatives, hex, strings including the empty one,
booleans, and enum cases in both spellings — the qualified form read as a case
when what precedes the dot is capitalised. `settings.retries` is left alone.
`U8(3)` stays excluded: nothing in the text tells a conversion from any other
call, so admitting it would admit `readConfig()` beside it.

Annotated declarations are skipped entirely — `@Native`, `@FFI.Syscall` and
`@Main` all mean the function is reached by something other than a Kira call.

### Known gap

`kira lint` on a package that does not compile exited 1 having printed nothing at
all. The `Err` arm says what happened now. **The same silence still happens when
the compile returns diagnostics rather than an error** — the path an unknown
annotation takes. Narrowed and not fixed: run `kira check` before trusting a
silent lint run.

## Diagnostics

`diagnostic-codes.tsv` is the single table, and the new `kira-diagnostic-registry`
crate generates `KiraError`, `kiraErrorFromCode` and the docs appendix from it,
with a gate that fails on drift. `Foundation`'s `Kira/DiagnosticCodes.kira` and
`Kira/Diagnostics.kira` are generated from the same table, so a Kira program reads
the codes the compiler emits.

The defect it closes was larger than recorded: **290 codes listed against 438
emitted, 129 in common** — 309 a program could not name, 161 names for codes
nothing emits, and 3 the enum listed that the lookup never answered. The table
carries 458 rows now. The drift gate was proven by making it fail.

`.gitattributes` forces LF in the working tree on every platform. Not a style
preference: the drift gate is a byte-for-byte comparison against what the
generator writes, and a Windows checkout with `core.autocrlf` on hands it CRLF.

## Macros

- **Hygiene.** `match` arms, `handle` arms and closure parameters each bind a name
  and none was renamed on expansion, so a template binding one captured any
  fragment mentioning it. Every name a template binds is renamed now, not only its
  statement binders.
- **Visibility.** Two modules of one package could each declare `mvPick` and the
  later file silently won, making meaning depend on read order. Now `KMAC031`,
  first declaration wins.
- **Macros were visible without an import.** An app importing `Outer`, which
  imports `Inner`, could call `innerDouble!(21)` and get `42`, while the same
  file's ordinary function was correctly refused `KSEM061`. Imports are read off
  the token stream, since expansion precedes the import table, and
  `ImportTable::sees` answers the question — one implementation, so macro
  visibility cannot drift from a function's.
- **A shadowed macro survived in other kind maps.** Three kinds, three maps,
  lookups by kind — a name shadowed by a nearer package was still reachable
  through a lookup of a different kind.
- **A macro name resolves the way every other name in the language resolves.**
- **Macro failures are loud and splices compose.**
- The registry is split into model, scanning and merging (`registry.rs` 291,
  `registry/model.rs` 145, `registry/scan.rs` 566, `registry/scan_tests.rs` 208).

## Sanitizers

`--sanitize` instruments the native code a build emits — the whole program on the
LLVM backend, the `@Native` half of a hybrid one — and links the managed bundle's
runtime against the sanitizer. **The pure VM is refused the flag by name** rather
than handed one that would watch nothing, because it interprets and keeps its own
exit accounting.

A hybrid manifest records what its halves were built with, so a launcher will not
load a library instrumented differently from the runtime opening it. LLVM
discovery learns to find a toolchain carrying compiler-rt, and the build scripts
provision one.

## Heap accounting

- **A VM run that exits with an unbalanced heap now fails**, rather than reporting
  success over a leak.
- **Native heap over-release is reported.**
- Frame releases are planned per engine heap model rather than shared between two
  that account differently.
- Native tasks are scoped to each run, and VM seam arguments are reclaimed when a
  host call errors.
- A native library stays mapped for as long as its code may run — an
  `dlclose` used to unmap a library out from under a thread the library itself had
  started.
- Callback identities survive VM hot reloads.

## Toolchain

- **The pinned LLVM moves to 23.1.0 final**, with arm64 bundles published.
- **libffi is republished as `v3.5.2-kira.2`**, built `-fPIC`: on x86_64 the
  previous `-fPIE` build emitted a direct `R_X86_64_PC32` to `ffi_type_float` that
  cannot go into a shared object. aarch64 routed through the GOT and was fine.
- Installers honor Cargo target directories; Apple runner archives are provisioned
  through `binstall`; the libffi and LLVM archives are recorded in the crates that
  need them.
- Unavailable package sources are reported rather than failing obscurely.
- An LLVM link line with no libraries on it is refused.
- Every library-name spelling is reduced the same way.
- Objects an interrupted build abandoned are swept.
- Each temporary source gets its own build directory, and a saved run is named by
  the process that saved it.
- Unauthorized macOS debugging is detected.
- `Foundation`'s `Kira/Toolchain.kira` exposes the toolchain to a Kira program.

## Defects found by pulling on red CI jobs

None of these came from a review reading the diff. Each had a green suite over it.

- **A compiler segfault on the Web target.** `LLVMAddAttributeAtIndex` does an
  unchecked `unwrap<Function>` and was handed a call instruction. It crashed only
  on Linux/aarch64; on macOS the same UB silently failed to attach the C ABI
  extension attribute — the mechanism that stops a callee reading a register whose
  high bits are the caller's leftovers. A live correctness hole everywhere,
  visible on one host. A call site's C extension is attached through the call-site
  API now.
- **An intermittent VM segfault**, 8% of runs, from the `dlclose` above.
- **libffi built `-fPIE`, not `-fPIC`.**
- **A host-encoded `char` expectation.**

## Networking

Before this a program could start a loopback protocol demonstration and read one
number back. It could not call a service: no start function took a URL, a header
or a body, and both TCP clients refused an `https` URI outright.

`api/tls.rs` owns every rustls configuration the crate builds over TCP: the
compiled-in Mozilla roots plus whatever DER anchors a caller adds, TLS 1.3 and
1.2, `ring` throughout because quinn already pins that provider. `HttpClient`
speaks `https` on both paths — the pooled one through hyper-rustls, the streaming
one through a plain-or-TLS stream the HTTP/1.1 and HTTP/2 handshakes share. An
HTTP/2 caller requires the peer to have selected `h2` rather than proceeding on an
unannounced connection.

`request.rs` is the payload-carrying C surface. A request is assembled against its
own handle and `send` consumes it into an ordinary operation handle, whose
response is read one selection at a time through a byte reader and a
Unicode-scalar reader. Reading rather than returning is what the seam allows:
`CString` is a parameter-only type, so a C function cannot hand text back, and a
`kira_rt_net_*` intrinsic family would put Tokio and rustls behind every native
program's runtime rather than behind an opt-in library.

`examples/llm` is a chat client against an OpenRouter-shaped API;
`examples/networking` exercises the loopback path.

### What the review round changed

- **A deadline that ended at the response headers.** `set_timeout_ms` documents a
  whole-request deadline, and both the C surface and the direct HTTP/3 client
  applied one only while the request was in flight — which ends when the headers
  arrive. A peer could answer, stall the body, and hold a caller open for as long
  as it kept the connection alive.
- **A body limit on the loopback server**, which previously read whatever a peer
  sent. It answers 413 past a limit, and answers a request whose body it never
  read.
- **A bounded client cache.** The pooled clients retained one entry per distinct
  trust-anchor set for the life of the process, and every loopback server presents
  a fresh certificate, so start/trust/send/close cycles accumulated clients and
  their pools without bound.
- **One `ClientConfig` per client.** Each connection rebuilt it, copying the
  Mozilla anchor set and giving every connection a private resumption store, so
  resumption never once applied.
- **A credential that could go out in cleartext.** An `OPENROUTER_BASE_URL`
  beginning `http://` put the bearer token on the wire. Refused now, rather than
  sent without the header, because a request that silently drops its credential
  fails far from its cause.
- **A POST that must not be repeated.** Retrying a chat completion after a timeout
  can buy a second completion and a second charge, and there is no documented
  idempotency key to make the repeat safe.

## Performance

**`Object::Str` held an owned `String` that `copy_value` deep-copied**, so
`charAt(i)` copied the whole string to read one byte and scanning cost length
squared: 3.36s against a 708 KB string where a 64-byte one took 0.27s. It holds an
`Rc<str>` now, as `Struct`, `Array` and `Enum` already did, and that parse takes
0.90s.

**The hybrid backend keyed its native half on the paths of the linked archives**,
so rebuilding one left the program answering with the C it was first built against
while the VM and native engines answered with the new. The key is a hash of the
archive bytes now, read in chunks; not cryptographic, because the question is
accident rather than forgery.

## Documentation and editors

The ten remaining top-level `docs/*.md` files are gone; `sites/docs` is the one
place user-facing language and toolchain behaviour is documented, and the
Zig-era installation and toolchain pages are retired. New pages include the
generated diagnostics code appendix, syscalls, C-layout values, WASM,
cross-compilation, toolchain installation and selection, profiling, debugging,
editors, concurrency, declarative macros, comptime functions, and the whole
Foundation section — JSON, filesystem, images, geometry, testing.

The execution-and-feature-status table is rewritten against what the tree actually
runs, with a Web column, and the shader target list now names GLSL 430, WGSL,
HLSL, MSL and SPIR-V.

The tree-sitter grammar moves with the syntax — 205 lines of `grammar.js` and the
corpus tests beside it — the Zed grammar pin points at the monorepo grammar, the
Zed extension builds outside the workspace, and Zed language features are wired.

## Tooling

`.config/nextest.toml` caps the groups that share a `target/debug` or a
`.kira-build`, because two of those tests at once corrupt what the other is
reading. `cargo test` cannot express a test group, so it races them and the
failure reads as a flake. **Run Rust tests with `cargo nextest run`.**

The Web pipeline reports itself unrunnable where Emscripten has no SDK, rather
than failing as though something were broken. The timeline tests take their
instants from what the test chooses rather than from sleeping.

## Verification

| Suite | Observed |
|---|---|
| Harness, `--backend vm` and `--backend llvm` run separately | 1544 each, identical case-name sets |
| Lifecycle harness, vm/llvm/hybrid | `20021` each |
| FFI harness, all three engines | 306 |
| `backend_parity` | 464 |
| Syscall harness / syscall parity | 19 on hybrid / 9 on all three |
| Diagnostics registry | 10 unit + 5 integration, 458 codes |
| Deadlock trap, all three engines under `timeout 10` | identical, one sentence |
| `cargo nextest run --workspace --exclude kira-cli` | 3388 passed |
| Full `--no-fail-fast` sweep, Linux/aarch64 | 4076/4080; the four are shader validators absent from that host |

## Review record

`@coderabbitai` could not review the largest branch: 829 files against a 100-file
limit. `@codex` reviewed it and the rounds that followed; every finding is
recorded in `.codex/work/codex-review-findings.md` as fixed with what, or not
fixed with why.

One is declined and the reason is worth keeping. The review asked for
target-specific archive paths — `target/aarch64-apple-darwin/debug/` and the like
— in place of the host-default `target/debug/` the example manifests name for
every triple. That is more obviously correct and it breaks every run of these
examples: CI builds the crate with a plain `cargo build -p kira-network` and then
runs both examples on each runner, and cargo writes a host build to
`target/debug`. A per-target path would name a file no build produces.
