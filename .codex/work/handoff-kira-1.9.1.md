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

- Harness on `--backend vm` and on `--backend llvm`, run separately: **1497**
  each, over identical case-name sets.
- Lifecycle harness on `vm`, `llvm` and `hybrid`:
  `main-thread-lifecycle / 42 / manual-main-thread / 20008`, exit 0 on each.
- FFI harness on `--backend hybrid`: **302**.
- `cargo test -p kira-cli --test backend_parity`: **457**, zero failures.
- `cargo test -p kira-diagnostic-registry`: 10 unit and 5 integration, over
  **443** codes. The drift gate was proven by making it fail, not by reading it.
- The receive-nothing-can-answer repro traps on all three engines inside
  `timeout 10`, one sentence between them.

The pins in `crates/kira-cli/tests/kik_harness.rs` are 1497, 20008 and 302, each
matching what the run reports.

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

- **Heap payloads refused.** Needs the value-tree representation the seam
  already has. `KSEM365` names the rule.
- **`Send` not enforced on payloads.** Not a hole today because every type the
  payload rule admits is `Send`; it becomes load-bearing with heap payloads and
  should land with them, beside the task-slot rule and `KSEM312`.
- **No test for cancellation while blocked**, nor for ordering against a
  lifecycle fiber specifically rather than against a task.

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
| 8 | Traits / async / tasks | Done: `CallableSignature`, contract diffs, generation-tagged handles, channels. Channel payloads are still one machine word; heap payloads open |
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

- **Heap payloads for channels** (section O). The rule is `KSEM365` and the
  shape is settled — see section 2. It is the one piece of the channel feature
  still missing, and the only remaining work that touches the runtime ABI.
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
- **Emscripten** installs at `~/emsdk` and works on aarch64 Linux:
  `./emsdk install latest && ./emsdk activate latest`, then
  `source ~/emsdk/emsdk_env.sh`. Without it the wasm end-to-end tests fail
  rather than skipping, so install it before trusting a green run.
- Pinned tallies live in `crates/kira-cli/tests/kik_harness.rs`: **1497** for
  the harness, 20008 for the lifecycle output, 302 for the ffi harness. The
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
