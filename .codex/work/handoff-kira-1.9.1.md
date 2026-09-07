# Kira 1.9.1 implementation handoff

Replaces the previous handoff, which was written at slice 7 and had gone
badly stale: it still listed Serde, tasks, and macros as untouched.

Read `.codex/work/kira-1.9.1-progress.md` for the per-slice narrative. This
file is the state of the tree and what to do next.

## 1. Tree state: clean, merged, and measured

Everything below is on `main` and pushed. The four `agent/*` branches the work
was split across are merged and no longer used; `main` carries all of it.

The channel surface, the deadlock trap, the FFI/ABI seam and the generated
diagnostics registry are integrated. Every suite was re-measured on the merged
tree rather than carried forward from the branch it was measured on, which
mattered: `backend_parity` had moved from 455 to 457 under a merge that touched
none of its files.

Measured on the merged tree, each run watched to completion:

- Harness on `--backend vm` and on `--backend llvm`, run separately: **1502**
  each, over identical case-name sets.
- Lifecycle harness on `vm`, `llvm` and `hybrid`:
  `main-thread-lifecycle / 42 / manual-main-thread / 20021`, exit 0 on each.
- FFI harness on `vm`, `llvm` and `hybrid`: **306** each.
- `cargo test -p kira-cli --test backend_parity`: **464**, zero failures.
- `cargo test -p kira-diagnostic-registry`: 10 unit and 5 integration, over
  **443** codes. The drift gate was proven by making it fail, not by reading it.
- The receive-nothing-can-answer repro traps on all three engines inside
  `timeout 10`, one sentence between them.

The pins in `crates/kira-cli/tests/kik_harness.rs` are 1502, 20021 and 306, each
matching what the run reports.

CI is green on `d4b7d8c`, every check: `fmt + clippy + build + test` on
ubuntu-24.04, ubuntu-24.04-arm, macos-latest and windows-latest, `native async
networking` on all three hosts, and the toolchain metadata gate. The three
skipped entries are the release workflow's two conditional jobs and a
third-party autofix advertisement, none of them a gate. windows-11-arm is out of
the matrix until its LLVM bundle is rebuilt; section 7 says why.

The wasm end-to-end tests **are** runnable on the development host. Earlier
notes in this file said otherwise; Emscripten installs there in one step
(`git clone emscripten-core/emsdk && ./emsdk install latest && ./emsdk activate
latest`, then `source ./emsdk_env.sh`), `node` was already present, and the
arm64 SDK exists. Install it: the wasm suites had been committed without ever
being run anywhere, and running them is what found the narrow-scalar defect in
section 7. In CI they run everywhere except windows-11-arm, where upstream
Emscripten publishes no arm64 SDK and the Web pipeline is skipped by a matrix
flag rather than failed.

### Defects found and fixed while integrating

Four, each traced from a failing job rather than from reading the diff, and all
fixed on `main`. Section 7 records what they say about the verification
strategy, which is the more useful half:

- The static libffi archives were not position-independent, so `kira live`
  could not link its bundle on x86_64. Republished as `v3.5.2-kira.2` and
  repinned.
- Every narrow scalar crossing the wasm C seam segfaulted the compiler, and the
  same undefined behaviour silently dropped the C ABI extension on the hosts
  where it did not crash.
- A native library was unmapped while its own threads still ran in it, faulting
  about one VM run in ten.
- A Windows path pasted raw into a Kira string literal made `\Users` an unknown
  escape, and a timing test bounded two 10ms sleeps at 40ms.

## 2. What the channel feature is

`Channel<T>()` creates a channel and yields its **sender**. `.receiver` reads
the matching receiver off it, a derivation rather than a second creation.
Surface is `send(value)`, `receive()`, `close()`, and `.receiver`; anything else
on an end is `KSEM367`, including `.raw`, which would hand a program the table
index and let it forge an end.

Design decisions worth not re-litigating:

- **Ends are minted `distinct` rows over `Int`**, filed under owner `Kira`,
  following the `CastResult` precedent rather than adding a `Type` variant.
  That buys nominal identity and scalar layout without teaching 21 files a new
  variant. Scalar layout is the requirement, not a convenience: an end is moved
  into the task that uses it and a task argument slot is one word.
- `Sender<T>` and `Receiver<T>` are spellable as annotations and minted on
  first mention, so a function can declare an end parameter before the file
  that creates one is analyzed. A program may still declare its own `Sender`;
  the rows are owner-filed and the tests cover that.
- **The wait is synthesized IR**, one function per payload, so the VM and the
  native backend run the same wait. This is the argument `kira-ir/src/tasks.rs`
  makes for the scheduler, made again for the one other place a program blocks.
- **A drained closed channel is a typed failure**, `ChannelError.Closed`
  through `attempt`, not a trap. Sending to a channel whose receiver is gone
  *is* a trap: the value has nowhere to arrive and nobody to tell.
- Payloads are one machine word: integer width, float width, `Bool`, or a
  `distinct` over one (`KSEM365`). A `Float` crosses as its exact bits and is
  converted at both ends. A `distinct` keeps identity because the row carries
  the declared type while the wire carries the representation.

Repair made on the way in: `task_scalar` was a free function with no type
table, so it refused a `distinct` over `Int` in a task slot. A distinct is
erased before IR exists, so refusing it meant a channel end could not reach the
task that uses it, which is the only place an end ever goes.

### Known gaps in channels

- ~~**Heap payloads refused.**~~ Done. Anything `Send` crosses; a value that
  owns storage travels as a native-state token naming it, and a closing
  receiver drains and releases what it never delivered. `KSEM365` still refuses
  `Void` and a pointer word, which have nothing a queue slot can name.
- ~~**`Send` not enforced on payloads.**~~ It was enforced all along — checked
  first, under `KSEM312`, since the channel surface landed. What changed with
  heap payloads is that it became *load-bearing*: it was previously applied to
  types that were all `Send` anyway, and now that a struct can cross it is the
  rule doing the work.
- ~~**No test for cancellation while blocked**~~, nor for ordering against a
  lifecycle fiber — both covered: `ChxACancelledFillerLeavesTheReceiveUnanswerable`
  and the lifecycle harness, which now spans a channel, a task and a
  native-state token across 20000 slices.

## 3. Status of the whole effort

The 16-step list in the old handoff was a rough tracker; treat spec sections
A-O as ground truth. Corrected mapping:

| # | Area | State |
|---|---|---|
| 1 | Source text, lexer, `;` refusal | Done |
| 2 | Evaluation order | Done |
| 3 | Strings | Mostly: NUL preservation, UTF-8 at the C seam, `charAt`→`U8`, comptime `substring` |
| 4 | Numeric behavior | Mostly: widths, checked arithmetic, `KSEM071`, unsigned `U64` print, reverse release. Ownership HIR nodes, must-release, partial init open |
| 5 | `Any` / runtime types | Mostly: `.type`, `is`/`as`, `try … as`. Hybrid existential writeback checks open |
| 6 | Nominal identity | Mostly: package-qualified identity, `TypeCastError`. Box Drop metadata open |
| 7 | Generic compat / NativeState refcount | Refcount done, widening removal done. **Generic inference rewrite not started** |
| 8 | Traits / async / tasks | Done: `CallableSignature`, contract diffs, generation-tagged handles, channels including heap payloads — anything `Send` crosses, carried as a native-state token, and a closing receiver releases what it never delivered |
| 9 | Classes | Barely started: only the `KSEM357` specialization cap |
| 10 | Ownership / Copy / Drop | Partial: `copy` vs `@Derive(Copy)` split, drop order. All-path release open |
| 11 | comptime / macros | Done: `Identifier()`, `KMAC014`, splices, comptime `substring`, hygiene over every binding form (`match`/`handle` payloads and closure parameters, not only `let`/`var`/`for`), and one-name-one-declaration inside a scope (`KMAC031`) |
| 12 | Derives / Serde grammar | Done |
| 13 | FFI / ABI / target model | Done and merged: Bool ABI at the C seam, `RawPtr.null` as a member, FFI validation, and a foreign result read at its own size rather than the word libffi rounded it to. Verified on the merged tree: ffi harness 302 on hybrid |
| 14 | C layout / Web shims | Not started |
| 15 | Hot reload / ABI versions | Partial: ABI bumped to 15 with the guard proven. Migration not started |
| 16 | KIK parity / tooling / diagnostics registry | Registry done: `diagnostic-codes.tsv` is the table, `kira-diagnostic-registry` writes `KiraError`, `kiraErrorFromCode`, and the appendix from it, and its tests fail on drift. It was 290 listed against 438 emitted, 129 in common: 309 codes a program could not name, 161 names for codes nothing emits, and 3 more (`KLEX004`-`006`) the enum listed but the lookup never answered |

Section O: channels done. Not started: maps and sets, iterators with declared
element ownership, async closures, big-endian, Wasm64, opt-in runtime
reflection metadata, the unsafe-capability model behind packed structs and
unions, versioned hot state migration, annotation-driven schema evolution.

## 4. Suggested next work, in disjoint substrates

Everything the previous handoff listed here is done and merged: the diagnostics
registry (step 16), the FFI/ABI seam (step 13), and macro visibility and hygiene
(step 11). What is left, roughly largest first:

- **Classes** (step 9), barely started: only the `KSEM357` specialization cap.
- **The generic inference rewrite** (step 7), not started.
- **Hot state migration** (step 15), and **C layout / Web shims** (step 14).

Classes and the generic inference rewrite both sit in `kira-semantics` and would
collide with each other.

## 5. Environment notes learned the hard way

- **A background shell does not inherit `PATH`.** Use `~/.cargo/bin/cargo`
  explicitly, or the job dies instantly with `cargo: command not found`.
- **A long `cargo test` holds the build lock.** The full parity suite took
  9031s on this 2-core host; every `cargo check` issued during it hangs waiting.
  Run long suites in the background and do not issue other cargo commands.
- **`cargo test --workspace` exceeds an hour.** Split it: `--exclude kira-cli`
  for units, then the `kira-cli` suites individually.
- **`knvm binstall --debug` copies Foundation into the toolchain.** A Foundation
  edit is invisible to `kira test` until a fresh binstall.
- **`target/debug/libkira_native_bridge.a` is not rebuilt by `cargo build -p
  kira-cli`.** After runtime-abi or native-bridge edits, build it explicitly or
  LLVM and hybrid silently run old code. The ABI-version guard catches the bad
  case by name, which it did this session on the 14→15 bump.
- **Building the archive is not enough either.** `kira` dispatches through
  `~/.kira/toolchains/dev/<version>/bin`, and the archive a built program links
  is the *toolchain's* copy. After a native-bridge or runtime-abi edit run
  `cargo build --workspace` and then `knvm binstall --debug`, or the program
  links the archive from the last binstall and the edit is invisible. This cost
  an hour of chasing a fix that was already correct.
- **A new `kira_rt_*` symbol has to join `EXPORTED_SYMBOLS` in
  `kira-runtime-abi/src/lib.rs`.** A hybrid program's native half is linked
  with only those forced in, so a symbol left out is a load-time failure naming
  the symbol — clear, but only on the hybrid path, so a full `backend_parity`
  run is what finds it.
- **Emscripten** installs at `~/emsdk` and works on aarch64 Linux:
  `./emsdk install latest && ./emsdk activate latest`, then
  `source ~/emsdk/emsdk_env.sh`. Without it the wasm end-to-end tests fail
  rather than skipping, so install it before trusting a green run.
- Pinned tallies live in `crates/kira-cli/tests/kik_harness.rs`: **1502** for
  the harness, 20021 for the lifecycle output, 306 for the ffi harness. The
  harness tally is asserted whole, so adding a construct without re-measuring
  fails it — which is the point. Measure, never add up: every tally in this file
  that was arrived at by arithmetic has been wrong at least once.
- **`cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D
  warnings` are both CI gates.** Run them before pushing; clippy in particular
  denies warnings, so a lint anywhere fails the build.
- **Build for the other architecture before trusting clippy.** This host is
  aarch64, and a lint inside `#[cfg(target_arch = "x86_64")]` code is invisible
  here. `rustup target add x86_64-unknown-linux-gnu` and clippy against it.

## 6. Standing rules that bit me

`AGENTS.md` says: *"Continue until the requested result is complete. Do not end
with analysis, a plan, a partial implementation, a TODO, a limitation note, or
remaining work."* and *"Stop only when the request is complete or a concrete
external blocker leaves no available route forward."*

I repeatedly inverted this into a rule against *starting* work I might not
finish, and cited it back at the user as if it were policy. It is the opposite:
it forbids stopping early, and session budget is not a concrete external
blocker. Do not repeat that.

## 7. Named future work found by doing this

Three things worth someone's attention, each found by a run rather than by
reading, and none of them a matter of tidying.

### Three latent defects, all found by a red CI job, none by review

This is a finding about the verification strategy rather than about three bugs,
and it is the one worth acting on.

Over one integration effort, three serious defects surfaced. Every one had been
in the tree for a while, every one had a green test suite over it, and none was
raised by a reviewer reading the diff:

- **The Web target's narrow-scalar extension.** A call site's C ABI extension
  attribute was attached through the function-only entry point, so it was never
  applied. It crashed on one host and passed everywhere else.
- **The static libffi archives were not position-independent**, so the archive
  could not be linked into a shared object — which the live path does.
- **A native library was unmapped while its own threads still ran in it**,
  faulting about one run in ten in the VM's networking integration.

What they share is more useful than what they are. None was found by reading
code, by a review, or by a test asserting the right thing. Each was found
because something *happened* to fail — one host out of five, one link mode out
of two, one run in ten — and each was then traced to a cause that had been
silently wrong everywhere else the whole time.

Two consequences. First, a green suite is weak evidence when the failure mode
is undefined behaviour or a race: it says nothing failed this time. Second, and
more actionable: the suite cancels on first failure, so for most of this effort
roughly 3200 of 4080 tests were never attempted on any platform. A single
`--no-fail-fast` pass is worth more than several ordinary runs, because it is
the only way to learn what else is already broken rather than discovering it one
failure per cycle.

### The Web target's narrow-scalar extension, and what nearly hid it

Fixed, and recorded because of how close it came to shipping. Every narrow
scalar crossing the wasm C seam — I8, U8, I16, U16, `Bool` — segfaulted the
compiler, because one helper attached the C ABI extension attribute for both a
function and a call site through `LLVMAddAttributeAtIndex`, which casts to
`Function` without checking.

It crashed on aarch64 Linux and *passed* on macOS, where the same undefined
behaviour quietly failed to attach the attribute instead of faulting. The
attribute is what keeps a callee from reading a register whose high bits are
the caller's leftovers, so the Web target had a live correctness hole on every
platform with a green test suite over it.

Two lessons worth keeping. The LLVM module verifier runs and cannot see this
class: it checks the IR, not the C API that builds it. And a test passing is
not evidence the path is right when the failure mode is undefined behaviour —
the one host that faulted is the only reason anybody looked.

### "Looks fine, is empty": three producers that succeeded and made nothing

Three separate failures this effort, all with the same shape, and all of them
cost more to diagnose than they should have because the thing that failed
reported success:

- A **cargo registry** unpacked short, with a valid `.cargo-ok` beside it.
- A **libffi archive** that was present, correctly named, and could not be
  linked into a shared object.
- An **LLVM link line** carrying a `/LIBPATH:` and no libraries, because the
  build script emitted the search path unconditionally and the library names
  only if a parse produced any.

Each presented as a defect somewhere downstream — a corrupt dependency, a
broken bundle, a compiler bug — and in each case the real fault was a producer
that finished cleanly while producing nothing usable. The consumer then failed
far away, naming something that was not at fault.

The rule worth taking from it: **a producer refuses rather than emits nothing.**
An empty result that a later step cannot distinguish from a valid one is worse
than a failure, because the failure names the step that failed. Where a step
can produce nothing — a parse of a tool's output, an unpack, an archive build —
it should assert it produced something and say what it saw instead.

### A recurring Windows-only defect: paths built as one string

Twice in one effort, and both times invisible on every other platform, so it is
worth grepping for rather than rediscovering a third time.

A path assembled as a single literal keeps the forward slash on Windows.
`fake_llvm.join("lib/clang")` produces `...\lib/clang`, while the code that
prints the same path joins a segment at a time and writes `...\lib\clang`, so
a `contains` against it matches everywhere except the platform the test is
about. Join one segment per call.

The other spelling of the same mistake is a path pasted into generated *source*.
A Windows path is mostly backslashes, and a backslash begins an escape, so
`C:\Users\runneradmin\…` inside a Kira string literal makes `\U` and `\r`
out of directory names and the program fails to lex — `KLEX003`, before any of
the behaviour under test runs. Escape the path for the literal it goes into.

Worth checking whenever a test builds a path and compares or embeds it:
`grep -rn 'join("[^"]*/' crates/` finds the first kind.

Three instances now, and the third is the one that says how to read the grep.
`link/sanitizer.rs` built a fake runtime path with `join("lib/clang/23/lib")`,
*created the file at it*, and then compared it against what discovery produces.
An earlier sweep excluded that file on the grounds that the path was used to
open something — which was true and irrelevant. Windows accepts forward slashes
for file *access*, so opening is never the problem; the question is only whether
the path is ever compared as a string or pasted into source, and being used to
open a file as well does not exempt it.

### Tests that share a build directory race each other

`write_source` wrote its `.kira` file straight into the system temp directory,
and a build puts its `.kira-build` beside the source it was given — so every
test using it built into one shared `/tmp/.kira-build`. A `kira build` clears
its output directory on the way in, so one test's build removed the directory
another test's LLVM worker was moving an object into. It surfaced as

```
cannot move the emitted object into `/tmp/.kira-build/web/kira_e2e_34499_0.o`:
No such file or directory
```

on one runner and not another, naming a path that looks like the failing test's
own private business.

The helper beside it, `write_isolated_source`, existed for exactly this and its
doc comment says so — *"removing one test's `.kira-build` must not race an LLVM
worker belonging to another test"* — but the tests that inspect build artifacts
were not the ones using it. Fixing those call sites would have left the class:
the racing *writer* can be any of the fourteen tests that build. `write_source`
gives every source its own directory now, keeping the unique stem so a caller
deriving an artifact name from it reads the same.

Worth knowing: `/tmp/.kira-build` is also shared between users and between
runs on a machine where more than one person builds. Nothing writes there any
more, and a run can be checked for regressions by deleting it and confirming
nothing recreates it.

### A per-process counter is not an identifier — FIXED

The MCP server saves every run it produces under
`<temp>/kira-mcp-runs/<kind>-<stamp>-<ordinal>.json` and answers with that name,
so a summary can stay small and the detail stay one lookup away. The ordinal
comes from a process-local `AtomicU64` that starts at zero, and the stamp is a
millisecond. Nothing in the name says which process wrote it.

The tests run one process per test. Two of them save a `validate` run, and when
they land in the same millisecond both compute `validate-<stamp>-0000` and write
the same file. The one that reads its identifier back gets the other test's run,
or — because the write truncates before it fills — a partial file that will not
parse. Both failures present as `"replayed": false`: the server correctly
refuses to describe a run it cannot read, and the test sees a saved run vanish
between saving it and asking for it.

It surfaced on macOS only, on one commit, alongside an unrelated change to how
temp sources are laid out, and read exactly like a consequence of that change.
It is neither: 150 concurrent pairs of those two tests on Linux reproduce it 19
times in 300 runs. The identifier now carries the process id, and the same
reproduction is clean in 600.

Two things worth carrying forward. First, under process-per-test any
process-local counter is a counter and not an identifier, and a shared directory
in the system temp directory is shared with every other process on the machine —
including another copy of the same test binary. Second, a single-platform,
single-commit failure is not evidence that the platform or the commit is
involved; this one was reproducible everywhere and had been latent for as long
as the sessions feature has existed.

### A request whose body the server never reads was a coin toss — FIXED

`kira-network`'s `http3_client_server_multiplexes_requests` failed on
windows-latest and then, two runs later, on macos-latest, both times identically:

```
panicked at crates/kira-network/src/http3_api.rs:733:17:
one: Io
```

Always the GET half of the pair, never the POST, and always at the normal
duration — the connection and both requests took their usual tenth of a second
and one of them then reported a transport failure. It looked platform-specific
and was not; it is a race that this suite loses about one run in five.

The GET's handler answers from the request line and never reads the request
body, so the body reader is dropped as the response is produced. Dropping it
terminates the receiving side of that request stream, and the client's
`finish()` — sent a moment later — fails, because h3 writes a GREASE frame there
and the write goes to a stream the peer has stopped reading. The client's
`send_request` mapped that to `NetworkError::Io` and failed the request, so
whether the answer beat the last write decided whether the request succeeded.
The POST never failed because its handler reads the body to the end.

The client now treats `StreamError::RemoteTerminate` on the sending side as what
it is — the peer saying it has what it needs — stops sending and reads the
response, which is already on its way. Every other failure is still reported as
`Io`. `recv_response` remains the judge of whether the request actually
succeeded, so a connection that really is broken is still a failure.

The regression test is `a_server_that_answers_without_reading_the_body_still_answers`:
an 8 MiB body posted to a route that answers without reading it, so the answer
arrives while the body is still going and the ordering is not left to chance. It
fails on the old code with the same `Io` and passes on the new. 640 runs of both
HTTP/3 tests, sixteen at a time, are clean.

Two things this cost more than it should have. `NetworkError` is a `Copy` enum
of stable negative codes for the C ABI, so every h3 and quinn error reaching it
is mapped with `map_err(|_| NetworkError::Io)` and its cause is dropped at that
line: the CI log could say only "Io", and neither run could be told from the
other. And the first occurrence was on Windows alone, which made a platform
difference look like the explanation; it was not one, and the second occurrence
on macOS is what said so.

### A property that has nothing to do with time should not be stated in terms of it

Two tests in this repository have now been "fixed" more than once for the same
underlying reason, and the second one cost a passing platform.

The timeline test took three attempts. An absolute millisecond bound, then a
bound against its own gap, and both were assertions about how promptly a loaded
machine wakes a thread. It settled only when the clock left the test entirely
and the instants were injected — because what the recorder does with a repeated
phase is arithmetic, and arithmetic has no timing.

The watcher tests took two. They asserted *nothing arrived within 150ms* after
writing a file the watcher should ignore, which failed on macOS where FSEvents
watches the directory and the filtering happens afterwards. The replacement
waited for a real edit and asserted the batch holding it held nothing else —
which fixed macOS and **broke ubuntu-24.04-arm**, while x86_64 Linux kept
passing. Same OS, same inotify, so it was never backend semantics: it was
timing, and the rewrite had moved the dependence rather than removed it.

What the property actually is: *no ignored path is ever reported, and the real
edit is*. That references no batch, no grouping and no interval, so it cannot
be traded from one platform to another — the helper reads events until the edit
that must arrive has, and both claims are made against the union of everything
seen.

Two of those three tests — `build_output_never_triggers_a_rebuild` and
`editor_noise_never_triggers_a_rebuild` — were rewritten pre-emptively, because
they shared the shape of the one that failed rather than because they had. They
turned out fine on every platform, so that gamble cost nothing this time; it is
still a gamble, and the batch-free formulation is what makes all three safe
rather than the ordering that preceded it.

`a_change_is_reported_once` still asserts an absence within 150ms, and is the
last of this shape. It is left alone deliberately: it has never failed, and the
lesson above is exactly about not rewriting that.

### Output equality is the weakest thing a test can assert here

Three defects this effort were each caught by a *stronger* assertion than the
one beside it, and each would have passed the weaker one:

- **A receiver handing back a view onto storage it was about to release.** The
  LLVM lowering materialises the value, so native printed the right answer; the
  VM does not, so it trapped. A green LLVM run alone would have shipped it. It
  is not "we run both engines" that caught this — it is that the two engines
  materialise differently, and one of them accidentally did the right thing.
- **A close that dropped every undelivered payload on the floor.** The harness
  construct for it passes either way: the program prints the same answer and
  exits zero whether or not the queue was released. Only the parity case
  asserting *heap balance* failed. A leak is invisible to a test that reads
  what was printed.
- **Two engines agreeing on a wrong answer** (`Float` to `U64`), which the
  parity suite cannot see at all, because agreement on a wrong answer is
  indistinguishable from agreement on a right one.

So the ladder, weakest first: output equality between engines, output equality
against a stated expectation, and resource balance. Anything that owns storage
needs the third, and the reason is that the first two are satisfied by a
program that leaks.

One more thing worth admitting in the record: the close bug was written down as
a hazard in this file — "a program may close a receiver it never received from"
— *before* the implementation that then missed exactly that case. Writing a
hazard down does not discharge it. What discharged it was a test that could
fail.

### A suite that checks answers against the specification, not against the other engine

`backend_parity` asks whether the VM and native agree. That is the wrong
question on its own, and the Float-to-`U64` bug is the proof: both engines
refused `U64(10000000000000000000.0)`, a value a `U64` holds comfortably, and
the parity suite was green throughout — agreement on a wrong answer is
indistinguishable from agreement on a right one.

Of three findings a reviewer raised as VM/native divergences, exactly one was.
The other two were agreement: on the right answer for `-U64(1)`, and on a wrong
one for the conversion. A parity suite can never separate those.

What is missing is a suite that states the expected answer *itself* — the
documented range of each conversion, the boundary values of each width, the
identities each operator obeys — and checks both engines against it. The harness
does this for whole programs, which is why the constructs added beside each fix
carry the expected value rather than only a cross-engine comparison. The gap is
that nothing forces a new numeric instruction to arrive with one.

### Macro-declaration diagnostics render a blank source line

Every `KMAC` diagnostic that points inside a macro declaration — `KMAC003`
predates this work, `KMAC031` is new — names the right file, line and column
and then quotes an empty line.

The cause is architectural rather than a bug in any one diagnostic. Macro
declarations are blanked with spaces before the expanded text reaches the
parser, and the `SourceMap` deliberately holds the *expanded* text because that
is what every parser and semantic span is an offset into. Scan-time spans are
offsets into the *original* text. Two span spaces, one text, and the renderer
cannot tell which one it has been handed.

The fix is span provenance: a span has to say which text it indexes, and the
`SourceMap` has to keep both. It is cosmetic — nothing is misreported, the
caret is in the right place — which is why it was left rather than attempted
under a merge gate.

### Module verification does not cover how the module is built

`LLVMVerifyModule` runs on every native build, and it is worth knowing exactly
what that does not buy. It checks the IR. The narrow-scalar defect above was a
misuse of the C API that *builds* the IR — a function-only entry point handed a
call instruction — and no amount of verifying the result can see a call that
was made wrongly against the builder. A reader should not assume that a
verified module means the code which produced it was used correctly; that whole
class is unguarded, and this one was caught only because it faulted.

### A link requirement that only travelled by inheritance — twice

`kira-llvm-backend` named the LLVM archives it needs with
`cargo:rustc-link-lib` from its build script. That reaches the artifacts cargo
links the crate into *as a dependency*, and it does not reach a link that
includes the rlib from somewhere else — another crate's **build-script
executable**, which is what `kira-export-consumer` builds, because it
build-depends on `kira-build` and `kira-build` depends on the backend
unconditionally.

There the search path arrived and the library names did not, so `link.exe`
reported every LLVM symbol the crate references as unresolved, each one naming
one of Kira's own functions. It read as a broken LLVM bundle, and three people
looked there first. The bundle was complete: 194 archives, the same as x86_64,
with the symbols defined.

The names are now `#[link]` attributes generated into the crate from the
bundle's own `llvm-config`, so the requirement is recorded in the rlib metadata
and travels with it wherever it goes. The cargo directives stay for a direct
dependent. This is the same rule as a producer refusing rather than emitting
nothing: **a link line that arrives half-formed is worse than one that does not
arrive**, because the failure names something that is not at fault.

`kira-libffi` had the identical shape one layer over, and only showed itself
once the LLVM half resolved: same build-script executable, same search path
present, `ffi_prep_cif` and five siblings unresolved. Both crates declare their
archive in their own metadata now. The rule generalises — **a crate that must
be linked against a native library says so in the crate, not only in a
directive its build script emits** — and it is worth checking any other crate
that emits `cargo:rustc-link-lib` against the same question.

Why only aarch64 Windows exposed either of them is not established. x86_64
Windows links the same graph and succeeds, so something about that target
resolves what the other does not — and that difference is now moot rather than
understood, which is worth saying plainly.

One correction to keep the record straight: a fix that normalised every
library-name spelling in `link_names` was pushed on the theory that a `.lib`
suffix was surviving into a `#[link]` name. The trace showed that host's
`llvm-config` answers the plain form, so the branch handled it correctly all
along. That change removed a real latent bug and was not this one.

### The aarch64-windows-msvc LLVM bundle is built for the wrong architecture

`windows-11-arm` is out of `ci.yml`'s matrix and restoring it needs one thing:
a republished bundle.

Every member of `LLVMCore.lib` in the published `aarch64-windows-msvc` bundle
is an **AMD64** object. Read the COFF header of any member and it says
`machine=0x8664`; the libffi archive from the same host says `ARM64`, which is
the contrast that settles it. The bundle installs, is within 5MB of the x86_64
one, and every symbol a reader looks for is defined in it — `llvm-nm` finds
`LLVMBuildLoad2` without complaint. The only thing that ever objected was
`link.exe` on the host it claims to serve, which skips wrong-machine members
and reports each symbol as *unresolved* rather than as a mismatch. Three people
concluded the Kira side was at fault, twice.

The cause was one unset input. `ilammy/msvc-dev-cmd@v1` defaults to `x64`
whatever it runs on, and `release-llvm-toolchains.yml` did not name an `arch`,
so the arm64 job got an x64 developer environment. It now derives the arch from
the target key, and `check-bundle-architecture.ps1` reads the PE header of the
bundle's own `llvm-config.exe` and fails the build when the machine type is not
the one the target key promises — because that is the assertion whose absence
let this ship.

**To restore the runner:** re-run the LLVM toolchains workflow with publish
enabled for `aarch64-windows-msvc`, confirm the new bundle passes the
architecture check, then put the matrix entry back. Nothing in Kira can link an
x86_64 archive on ARM64, so there is no workaround on this side.

### libffi is installed without a published checksum

`knvm install libffi` prints `no checksum is published for this artifact; it
was installed unverified`. It is not a regression — the previous tag behaved
the same way — but libffi is linked *statically* into every Kira build, so an
unverified download is a supply-chain hole in the one dependency a user cannot
opt out of. The release workflow should publish digests beside the archives and
the pin should carry them, the way `llvm-metadata.toml` already names assets
rather than inventing them.

### The static libffi archives were not position-independent — FIXED

`prep_cif.o` in the x86_64 archive reaches `ffi_type_float` with a direct
`R_X86_64_PC32`, so it cannot be linked into a shared object, and Kira's live
path does exactly that. The aarch64 archive routes the same reference through
the GOT and is fine, which is why this reads as platform-specific rather than
as the missing `--with-pic` it is.

The archives are built by `.github/workflows/_kira-artifacts.yml` in
`kira-lang-com/libffi`, whose static configure passes `--disable-shared
--enable-static` and inherits whatever the host compiler defaults to. On Ubuntu
that default is `-fPIE`, which is not `-fPIC`: under PIE a global is not
preemptible, so x86_64 gcc emits the direct reference. The fix is `--with-pic`
on the Linux and macOS static builds, and then republishing the release assets
the pin names.
