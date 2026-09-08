# Kira 1.9.1

Kira 1.9.1 gives a program channels between its own contexts, a request surface
that can call a real service over TLS, a JSON reader in Foundation, and a lint
system with levels and groups that actually sees the code it is pointed at. The
FFI seam learns Bool, a written null pointer, and a C header's enumerators. The
Linux system-call table grows from fourteen entries to fifty-one — enough for a
program that supervises processes and drives a device rather than one that only
reads and writes. Diagnostics are generated from one table with a drift gate.

It also fixes a compiler segfault on the Web target, an intermittent VM
segfault, a libffi built with the wrong relocation model, macro hygiene and
visibility holes, and a string representation that made scanning cost length
squared.

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

    @Main function main() {
        let tx = Channel<Int>()
        let rx = tx.receiver
        var pending = Task { fill(tx) }
        attempt {
            let first = try rx.receive()
            …
        }
    }

Ends are minted `distinct` rows over `Int`. That buys nominal identity and
scalar layout, and the layout is the requirement rather than a convenience: an
end is moved into the task that uses it, and a task argument slot is one word.

The wait is synthesized IR, one function per payload, so both engines run the
same wait rather than two that can drift. A drained closed channel is a typed
failure through `attempt`; sending to a channel whose receiver is gone is a trap.

**A receive on an empty channel with nothing runnable used to hang forever.** It
now raises `ChannelTrap::Deadlock` — deliberately not `Closed`, which would
claim the sender went away when it did not.

**Heap payloads** cross on the existing native-state transport: send boxes the
value and queues the token, receive materialises and releases. No new runtime
symbol and no ABI bump, because a token is one word and the channel primitive
contract already carries words. `native_state_type_id` derives from the type's
shape, so the sender's box and the receiver's recovery agree by construction
rather than by protocol. Closing a receiver drains and releases what is still
queued.

`KSEM365` refuses a pointer payload because an address means nothing in the
receiving context. It now asks that of the whole value — across structs, arrays,
enum payloads, distincts and cells — and names the pointer it found, so
`Channel<Envelope>` carrying a `RawPtr` field is refused the way
`Channel<RawPtr>` always was.

A hybrid program keeps **one** channel table. A `@Runtime` `main` creating a
channel and passing the sender to a `@Native` function used to trap with
`channel end handle is not live`, because the two halves kept separate tables.
The bytecode half now performs channel primitives on the native half's table
through `HostCapabilities::channel_op`, which returns `None` by default so every
other host keeps its own. The archive gains `kira_rt_channel_try`, which answers
a trap code rather than exiting, because the VM raises its own trap.

Undelivered boxed payloads no longer leak at teardown. The table is told at
creation whether a queued word is a token, and hands them back when emptied. The
generated entry ends the channel scope in the function that *started* it: the
table is thread-local, and under a native event loop the thread `@Main` runs on
is not the one `main` returns on, so a reset in `main` emptied a table nothing
had filled.

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
rule is whether the call is bounded by a descriptor the program already holds,
so `mmap`, `clone` and `getpid` are refused there with a reason rather than
answered wrongly — `mmap` would map into the interpreter's address space,
`clone` would fork the interpreter, `getpid` would answer with its identity.

## The FFI seam

**Bool crosses the C seam**, and the fix is not cosmetic. On LLVM 23.1.0,
`llc -O2` on `load i1` emits a bare `ldrb` with no normalization, so a `_Bool`
byte of 2 becomes an "i1" holding 2. The `i8` + `icmp ne` form emits `ldrb` +
`cmp`. The parity fixture presents a deliberately non-canonical `_Bool` byte of
2 and confirms it survives both by value and behind a pointer.

**`RawPtr.null`** is something a program writes and compares, rather than a cast
from zero. Foreign declarations the seam cannot honor are refused at analysis
time rather than at link time.

**Float→U64** gained `ConvertFloatToUInt`, the unsigned twin of a conversion that
already existed on the other side. Both engines previously trapped on `1e19`,
which fits `U64`.

### Autobinding carries a header's enumerators

A binding module held structs, functions, arrays, pointers and callbacks, and
had nowhere to put a number. So a package generating its types from a header
still wrote that header's constants out by hand — `DRM_IOCTL_MODE_SETCRTC` and
every value like it was a hexadecimal literal in Kira source that nothing
checked against the header it came from.

Enumerators now come across as module-scope `let`s, under the same rule the rest
of the seam follows: every one the listed headers define under `AllPublic`, or
the named ones under a `constants` selection. The value is what clang evaluated
for the target being built, so an enumerator whose width or sign depends on the
platform is read rather than assumed.

The enum type itself is deliberately **not** declared. Kira's enum is a tagged
union with payloads and exhaustive matching; C's is a set of integers in a
shared namespace that callers freely combine with `|`. Binding one as the other
would promise a totality C never had, so the values cross and the type does not.
Selection is therefore by enumerator, never by the enum that holds it — which is
also the only thing that works for the anonymous enums headers use to write bare
constants.

A `#define` still cannot be bound, and now says so. The preprocessor has
consumed it before there is a cursor to walk, so a declaration naming one gets
the same recorded reason as any other declaration the seam cannot carry, rather
than silently producing nothing.

## Networking: a request surface over TLS

Before this a program could start a loopback protocol demonstration and read one
number back. It could not call a service: no start function took a URL, a header
or a body, and both TCP clients refused an `https` URI outright. HTTP/3 was the
only encrypted path, and it verified against a certificate the loopback server
had generated, so nothing in the crate could reach a public root.

`api/tls.rs` now owns every rustls configuration the crate builds over TCP: the
compiled-in Mozilla roots plus whatever DER anchors a caller adds, TLS 1.3 and
1.2, `ring` throughout because quinn already pins that provider. `HttpClient`
speaks `https` on both paths — the pooled one through hyper-rustls, the streaming
one through a plain-or-TLS stream the HTTP/1.1 and HTTP/2 handshakes share. An
HTTP/2 caller requires the peer to have selected `h2` rather than proceeding on
an unannounced connection.

`request.rs` is the payload-carrying C surface. A request is assembled against
its own handle and `send` consumes it into an ordinary operation handle, whose
response is read one selection at a time through a byte reader and a
Unicode-scalar reader. Reading rather than returning is what the seam allows:
`CString` is a parameter-only type, so a C function cannot hand text back, and a
`kira_rt_net_*` intrinsic family would put Tokio and rustls behind every native
program's runtime rather than behind an opt-in library.

The HTTP/3 halves are split into `http3/`: a program that dials a service reads
one half and a program that answers reads the other.

`examples/llm` is a chat client against an OpenRouter-shaped API;
`examples/networking` exercises the loopback path. Both print the same answer on
all three backends.

### What the review round changed

- **A deadline that ended at the response headers.** `set_timeout_ms` documents a
  whole-request deadline, and both the C surface and the direct HTTP/3 client
  applied one only while the request was in flight — which ends when the headers
  arrive. A peer could answer, stall the body, and hold a caller open for as long
  as it kept the connection alive. Both now carry one absolute deadline across
  the send and the body.
- **A body limit on the loopback server**, which previously read whatever a peer
  sent and could be taken down by writing. It answers 413 past a limit.
- **A bounded client cache.** The pooled clients retained one entry per distinct
  trust-anchor set for the life of the process, and every loopback server
  presents a fresh certificate, so start/trust/send/close cycles accumulated
  clients and their pools without bound. Custom-root entries are capped and
  evicted oldest-first.
- **One `ClientConfig` per client.** Each connection rebuilt it, copying the
  Mozilla anchor set and giving every connection a private resumption store, so
  resumption never once applied.
- **A credential that could go out in cleartext.** `baseUrl` comes from the
  environment, so an `OPENROUTER_BASE_URL` beginning `http://` put the bearer
  token on the wire. Refused now, rather than sent without the header, because a
  request that silently drops its credential fails far from its cause.
- **A POST that must not be repeated.** Retrying a chat completion after a
  timeout can buy a second completion and a second charge. There is no
  documented idempotency key to make the repeat safe, so it sends once.

## Foundation: JSON

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
- **An integer range that stopped one short.** `Int`'s minimum has a magnitude
  one larger than its maximum, so measuring a literal before applying its sign
  refused `-9223372036854775808`, which fits. The accumulator runs downwards now
  and the cutoff is 2^63 exactly.
- **An exponent the peer chooses.** Scaling stepped through it 22 multiplications
  at a time, so `1e1000000` cost about a million of them to reach an infinity it
  was always going to reach. Clamped to the range a `Float` can still notice.
- **NaN.** `jsonInt` refuses what it cannot convert by comparing against `Int`'s
  bounds, and every comparison against NaN is false, so it passed both checks and
  reached a conversion that traps. It answers the documented fallback now.
- **`readKeyword` blamed the wrong byte.** `trxe` reported the offset of the `t`;
  it refuses at the byte that broke the keyword, one byte at a time.
- **`writeJson` wrote a whole `Number` as `3`**, and `3` reads back as an
  `Integer`. `Integer(3)` and `Number(3.0)` are different values of the same enum
  — the distinction those variants exist to keep. A trailing `.0` costs two bytes
  and preserves it.

The documentation no longer claims a round trip for infinities and NaN, which are
written `null` and read back as `Null`. Qualified rather than removed, because it
holds for every finite number.

## Lints

**A lint could not see most of a Kira program.** A lint walks statements, and
`statements_of` answered for a function and returned nothing for every other
form. The kik harness is written as constructs, so every structural lint had run
over the corpus the compiler is judged against and reported nothing, for as long
as the corpus has existed.

Three layers were blind, not one: `statements_of` read only a function's body;
`procedural::top_level` never scanned `trait` or `extend`, so neither reached a
macro at all; and `DeclarationKind` had no variant for either, so anything that
did arrive was dropped as `Other`. A body is now read from a function, a struct's
methods, a class's methods, a construct's members and initializers, an `extend`
block, and a trait's written defaults. `extend` is matched by text, because it is
a contextual keyword with no token of its own.

That found **107 manual index loops where it had found none**. 105 are rewritten
by `kira lint --fix`; the harness passes identically on vm and llvm at 1544 cases
before and after. The two left are tests *about* `while`, and want a per-site
allow rather than a rewrite.

### Levels

`enabled: Bool` beside `severity: String` made `enabled = false, severity =
"error"` a state that parses, reads as emphatic, and means nothing. Whether a
lint runs and how loudly it speaks are one question. **`LintLevel` is `Allow |
Warn | Deny`**, which is what `kira-linter`'s own Rust enum has always been —
only the surface a package writes disagreed.

Every lint has a level before any package says anything, so `kira lint` in a
package that configured nothing runs the standard set. **It used to run nothing
and print `ok`**, which is the shape of a fake success. A `linter.kira` states
what differs: a `LintDefaults` entry moves them all, a `Lint` entry moves one,
and entries are matched by their `code` rather than by the declaration's name —
matching on the name made it load-bearing, so renaming an entry silently turned
its lint off.

### Groups

A level says what a finding costs; a group says whether the lint is asked at all.
Conflating them meant `--lint-level=deny` could shout about what already ran and
could never turn on what did not, which is the opposite of how `-W pedantic`
works everywhere else.

**`LintGroup` is `Correctness | Style | Complexity | Perf | Pedantic |
Restriction`.** Everything but the last two runs by default. A package asks for
more with `let groups: [LintGroup] = [.Pedantic]`.

That reclassifies two lints honestly: the file-length ceiling is `Restriction` —
a policy a project chooses, not a defect anyone would recognise — and the
constant-function lint is `Pedantic`.

### The command line, and a single site

`--lint-level=<level>` moves every code; `--allow`, `--warn` and `--deny` each
move one, so CI can deny what a package warns without editing the package. It
escalates what was reported and cannot resurrect a lint the package allowed,
because an allowed lint never ran. Without a flag the policy passes everything
through unchanged: a default of `Warn` applied over the runner's own decisions
would quietly demote every denied lint, so "nobody asked" is its own state.

`@Allow(KLINT002)` above a declaration turns one lint off for it alone. Some code
is the exception a lint is written to find and is right anyway. Annotations reach
a macro for the first time here, as the list of what was written rather than as
the declaration's text — searching the text would find `@Allow(KLINT002)` inside
a string in the body just as readily.

### New and changed lints

**KLINT004** finds a zero-argument function whose whole body returns a literal.
`function limit() -> Int { return 700 }` is a constant spelled as a call, and
Kira has module-scope `let`, so it can be one. Narrow on purpose: zero
parameters, exactly one statement, that statement a `return`, and the returned
text a literal. It covers integers, floats, negatives, hex, strings including the
empty one, booleans, and enum cases in both spellings — `.Red` and `Colour.Red`,
the qualified form read as a case when what precedes the dot is capitalised,
which is how Kira spells a type. `settings.retries` is a field of something and
is left alone. `U8(3)` stays excluded: nothing in the text tells a conversion from
any other call, so admitting it would admit `readConfig()` beside it.

Annotated declarations are skipped entirely — `@Native`, `@FFI.Syscall` and
`@Main` all mean the function is reached by something other than a Kira call, and
`packages/linux` is written exactly this way.

**KLINT002** also required a `.count` bound, so a loop counted against a literal
was never reported. A literal cannot change under the loop, which is the only
property the rewrite needs. A variable still does not: it might change, and this
lint does not guess. KLINT002 also emitted a warning whatever it was configured
as, so a package that denied it got a warning and a build that passed.

### Known gap

`kira lint` on a package that does not compile exited 1 having printed nothing at
all. The `Err` arm now says what happened. **The same silence still happens when
the compile returns diagnostics rather than an error** — the path an unknown
annotation takes. Narrowed and not fixed: run `kira check` before trusting a
silent lint run.

## Diagnostics

`diagnostic-codes.tsv` is the single table, and the new `kira-diagnostic-registry`
crate generates `KiraError`, `kiraErrorFromCode` and the docs appendix from it,
with a gate that fails on drift.

The defect it closes was larger than recorded: **290 codes listed against 438
emitted, 129 in common** — 309 a program could not name, 161 names for codes
nothing emits, and 3 the enum listed that the lookup never answered. The table
carries 458 rows now. The drift gate was proven by making it fail, not by reading
it.

`.gitattributes` forces LF in the working tree on every platform. Not a style
preference: the drift gate is a byte-for-byte comparison against what the
generator writes, and a Windows checkout with `core.autocrlf` on hands it CRLF.

## Macros

**Hygiene.** `match` arms, `handle` arms and closure parameters each bind a name
and none was renamed on expansion, so a template binding one captured any
fragment mentioning it.

**Visibility.** Two modules of one package could each declare `mvPick` and the
later file silently won, making meaning depend on read order. Now `KMAC031`,
first declaration wins — following the existing "one program is one flat scope"
rule rather than inventing one for macros.

**Macros were visible without an import.** An app importing `Outer`, which
imports `Inner`, could call `innerDouble!(21)` and get `42`, while the same
file's ordinary function was correctly refused `KSEM061`. Imports are now read
off the token stream, since expansion precedes the import table, and
`ImportTable::sees` answers the question — one implementation, so macro
visibility cannot drift from a function's.

**A shadowed macro survived in other kind maps.** Three kinds, three maps,
lookups by kind — a name shadowed by a nearer package was still reachable through
a lookup of a different kind.

## Class dispatch

Both class harness files carried a parity rule: no inherited base method may call
`self.<m>()` where a descendant overrides `<m>`, because that dispatch was
virtual on vm and hybrid and static on llvm. Both steered around the shape
entirely, so **the language's most obvious inheritance behaviour was the one thing
untested**.

It does not diverge. Classes are monomorphized: a parent's body is registered
once per descendant with the receiver typed as the concrete class, so `self` is
statically the leaf, and static and virtual dispatch are the same dispatch on
every backend. Measured on vm, llvm and hybrid across seven cases — an inherited
body reaching a descendant's override, two hops of it, an override calling up to
a grandparent whose body dispatches back down through `self`, the same across two
parents, and an instance reached through an array rather than a binding.

## Defects found by pulling on red CI jobs

None of these came from a review reading the diff. Each had a green suite over it.

- **A compiler segfault on the Web target.** `LLVMAddAttributeAtIndex` does an
  unchecked `unwrap<Function>` and was handed a call instruction. It crashed only
  on Linux/aarch64; on macOS the same UB silently failed to attach the C ABI
  extension attribute — the mechanism that stops a callee reading a register whose
  high bits are the caller's leftovers. A live correctness hole everywhere,
  visible on one host.
- **An intermittent VM segfault.** `dlclose` unmapped a library out from under a
  thread the library itself had started. Only the VM opens a library at run time.
  8% of runs.
- **libffi built `-fPIE`, not `-fPIC`.** On x86_64 the compiler emitted a direct
  `R_X86_64_PC32` to `ffi_type_float` that cannot go into a shared object;
  aarch64 routed through the GOT and was fine. Republished as `v3.5.2-kira.2` and
  repinned.
- **A host-encoded `char` expectation.** `plain_char_spelling()` decided from
  architecture alone, but Apple and Microsoft override `char` to signed on every
  architecture they ship.

## Performance and correctness in the runtime

**`Object::Str` held an owned `String` that `copy_value` deep-copied**, so
`charAt(i)` copied the whole string to read one byte and scanning cost length
squared: 3.36s against a 708 KB string where a 64-byte one took 0.27s. It holds
an `Rc<str>` now, as `Struct`, `Array` and `Enum` already did for this reason,
and that parse takes 0.90s.

**The hybrid backend keyed its native half on the paths of the linked archives**,
so rebuilding one — the workflow both FFI examples document — left the program
answering with the C it was first built against while the VM and native engines
answered with the new. The key is now a hash of the archive bytes, read in
chunks; not cryptographic, because the question is accident rather than forgery.

## Documentation and editors

The ten remaining top-level `docs/*.md` files are gone; `sites/docs` is the one
place user-facing language and toolchain behaviour is documented. New pages
include the diagnostics code appendix (generated), syscalls, C-layout values,
WASM, cross-compilation, toolchain installation and selection, profiling,
debugging, editors, concurrency, declarative macros, comptime functions, and the
Foundation section — JSON, filesystem, images, geometry, testing.

The tree-sitter grammar moves with the syntax: 205 lines of `grammar.js` and the
corpus tests beside it, covering attributes, comments, declarations, expressions,
functions, literals, macros, statements and traits.

## Tooling

`.config/nextest.toml` caps the groups that share a `target/debug` or a
`.kira-build`, because two of those tests at once corrupt what the other is
reading. `cargo test` cannot express a test group, so it races them and the
failure reads as a flake. **Run Rust tests with `cargo nextest run`.**

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
