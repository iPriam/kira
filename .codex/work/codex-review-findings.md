# @codex review of the 1.9.1 integration: what each finding got

AGENTS.md requires a completed AI review before upstreaming, and a record of
each finding as fixed or not fixed with the reason. @codex reviewed `9b5a405`
and raised seven P1s. Every claim below was checked by running it, on the
merged tree, before it was accepted or rejected — three of the seven were
framed as VM/native divergences and only one of those is one.

## 1. Unary negation of a nonzero U64 — NOT FIXED, the finding is wrong

The claim: `-U64(1)` does not trap where it should, because the signed
subtraction only overflows on the raw `i64::MIN` pattern and `check_width`
skips every 64-bit spelling.

Measured, `let a: U64 = 1  let b = -a  print(b)`:

- `--backend vm`: `18446744073709551615`, exit 0
- `--backend llvm`: `18446744073709551615`, exit 0

The engines agree, so this is not a divergence. Nor is the answer wrong.
`sites/docs/content/docs/language-guide/basic-operators.mdx` documents exactly
this shape under Bitwise Operators:

```kira
let signed: Int = -1
var unsigned: U64 = 0
unsigned = unsigned - 1   // the same 64 bits
```

A `U64` holding the all-ones word is the documented model, not an overflow that
escaped a check. Trapping here would refuse the idiom the language guide
teaches. Left alone deliberately.

## 2. Bitwise complement not normalized at the declared width — FIXED

The claim: `~U8(0)` traps on the VM and does not on native. Confirmed:

- `--backend vm`: `kira: runtime trap: integer overflow: the result does not fit
  ` U8` `, exit 1
- `--backend llvm`: `-1`, exit 0

A real divergence over a one-token expression. The cause was not a missing
normalization on the LLVM side but an extra check on the VM side: the IR
already answers `Int` for `BitNot` whatever width it was handed, the LLVM
backend read that, and the bytecode compiler range-checked the *operand's*
width instead.

Fixed by giving the rule one home — `kira_ir::unary_result_type` — that the IR
and the bytecode compiler both read, so neither can decide it alone again. All
three engines now answer `-1`.

The harness had no coverage of `~` at any width anywhere; six constructs in
`NwxNumericWidthTests.kira` and a parity case across vm, llvm and hybrid.

## 3. Float to U64 above 2^63 — FIXED

The claim: the destination funnels through the signed-only `lower_float_to_int`,
whose accepted interval ends at 2^63.

Measured, `U64(10000000000000000000.0)` — a value that fits a `U64`, whose
maximum is about 1.8e19:

- `--backend vm`: traps, `has no integer value`
- `--backend llvm`: traps, the same sentence

The engines agree, so this is not a divergence either — it is agreement on the
wrong answer, on both paths, and the finding's diagnosis of the LLVM path is
right about the cause.

### The ABI version was deliberately not bumped

An ABI bump was asked for alongside this fix and was not made. That is a
decision, not an oversight.

`op/opcode.rs` states it directly, twice, beside earlier additions: *adding an
opcode is not an ABI change.* And `RUNTIME_ABI_VERSION`'s own documentation
scopes what the marker is for — generated native code and the runtime archive
are built separately, and if an archive was built before *a signature changed*
the symbols still resolve by name and the mismatch is silent. The version is
baked into a symbol so that link fails instead.

This change adds no runtime symbol and alters no signature. The VM gets an
opcode; the native side emits inline IR that calls the `trap_float_to_int` that
was already there. Bumping to 16 would force every archive to rebuild for a
non-event, and spending the marker on changes it does not guard is what makes it
easier to ignore the next time it does.

## 4. Parsing the minimum Int overflows — FIXED

`foundation/app/SerdeText.kira` accumulated the positive magnitude and applied
the sign afterwards. The magnitude of the most negative Int is one past the most
positive one, so `-9223372036854775808` overflowed on the way in — on its own
round trip, through checked arithmetic that traps.

Accumulating downward instead reaches every value of the type. A positive value
too large to hold still traps, on the negation at the end, which is where the
overflow actually is. Three constructs in `DnxDistinctTests.kira` cover the two
bounds and the ordinary values around zero.

## 5. Same-line handler payloads missed by the hygiene fix — FIXED

The claim: `handle { TooBig(reason) { … } }` on one line leaves the payload
unrenamed, because the block form asked for a line break.

Correct, and a hole in the fix it reviewed. A handler arm begins a statement,
and a statement begins after a brace as well as after a line break — the arm
list's own `{`, or the `}` that closed the arm before it. That is what tells it
from `if ready(flag) { … }`, whose call follows a keyword. Two unit tests, one
for the single-arm form and one for two arms sharing a line, and a harness
construct for the same-line spelling specifically.

## 6, 7. File-size ceiling violations — FIXED

AGENTS.md: never leave a `.rs` file at or above 1000 lines, and split into
cohesive 300-500 line modules preserving APIs, behavior and layering.

`crates/kira-vm-runtime/src/lib.rs` was 1,163 lines, of which 1,090 were one
inline `mod tests`. The crate already keeps its test modules in their own files
(`compiler_tests.rs`, `release_tests.rs`, and four more), so the split follows
what was there rather than inventing a scheme: `vm_test_support.rs` for the two
fixtures every module needs, then `debug_tests.rs`, `native_seam_tests.rs`,
`numeric_tests.rs` and `program_tests.rs` grouped by what they exercise. The
crate root is 90 lines and the same 116 tests pass.

`crates/kira-macros/src/decl.rs` was 1,074 lines doing three jobs. Split the way
`eval.rs` already splits: `decl/model.rs` holds what a macro is handed when it
reflects, `decl/scan.rs` holds the locating scan, and `decl.rs` keeps the entry
points and the tests. 143 tests pass.

The split exposed a pre-existing documentation defect: `scan_distinct`'s doc
comment sat above `starts_declaration`, so one function carried another's
explanation and `scan_distinct` had none. Each now documents itself.

No file in the repository is at or above 1000 lines.

## Found by the review round, but not by the review

Worth recording here because it is the most serious defect this branch has
carried, and no reviewer raised it — a red CI job did.

**Every narrow scalar crossing the wasm C seam segfaulted the compiler.** I8,
U8, I16, U16 and `Bool`; I32, I64, F32, F64 and `RawPtr` were fine. That split
is exactly the set `foreign_c_extension` answers `Some` for, and it answers
`None` on every non-wasm target, which is why this was wasm-only.

One helper attached the C ABI extension attribute for both a function
declaration and a call site, through `LLVMAddAttributeAtIndex`. That entry
point casts to `Function` without checking, so handing it the result of
`LLVMBuildCall2` writes through a pointer to something that is not one. A call
site takes its attributes through `LLVMAddCallSiteAttribute`. They are two
methods now, because the caller always knows which it holds.

**The crash was the lesser half.** The same undefined behaviour did not fault
on macOS — the test passed there — which means the extension attribute was
silently never reaching the call site. That attribute is the whole mechanism
keeping a callee from reading a register whose bits above the value are
whatever the caller last left in it. So the Web target had a live correctness
hole on every platform, and every test was green about it. The crash happened
on one host, by luck, and is the only reason it was found at all.

Two things follow. The LLVM module verifier is run and did not catch this:
it checks the IR, and this was a misuse of the C API that builds it, which is
outside what the verifier can see. And `LLVMAddAttributeAtIndex` has exactly
two call sites — the one above, and one in `address_sanitize` that walks the
module's own function list and is correct — so this was the only instance,
checked rather than assumed.

## What the review changed about how the rest was done

Three of the seven findings were framed as VM/native divergences and one was.
The other two were agreement — in finding 1 on the right answer, in finding 3 on
a wrong one. That distinction is only visible by running both engines, and the
parity suite cannot see finding 3 at all, because agreement on a wrong answer
reads exactly like agreement on a right one. Each finding here was measured
before it was accepted.

# Second @codex review, on `01b22c6`: six findings

All six were checked by running them before anything was changed. All six were
right, which the first round's seven were not: two of those were wrong and
proved so. The difference is worth naming — this round's findings are about
what a program *can reach*, and every one of them turned out to be reachable.

## 8. A hybrid program kept two channel tables — FIXED

**Verified by running it.** A `@Runtime` `main` creating `Channel<Int>()` and
handing the sender to a `@Native` function traps:

```
kira: runtime trap: channel end handle is not live
```

A channel end is one machine word, so it crosses the seam like any other
scalar and the type checker lets it; what it names is a row in a table, and
each half kept its own.

The bytecode half now performs its channel primitives on the native half's
table, the way it already reads and writes native state there — a host
capability answering `None` by default, so every host but a hybrid session's
keeps its own table. The archive gained `kira_rt_channel_try`, which answers a
trap code instead of ending the process, because the VM raises its own trap
rather than exiting.

**Half of the finding was wrong, and it is the half worth recording.** It also
claimed "boxed payloads would resolve against a different native-state store in
the opposite engine". They do not: `Session::vm_state` is `Some` only when
*every* function is `@Runtime`, so a program with a native half already routes
its state to the library's store. One token domain, already. The channel table
was the only split one.

Four FFI-harness cases now cross the seam in both directions, one of them with
an owned payload. None of the 302 cases already there could have failed on
this: every one of them used both ends on the same side.

## 9. Undelivered boxed payloads leaked at run teardown — FIXED

**Verified by running it.** Three strings queued and never received, on the
LLVM backend under `KIRA_HEAP_REPORT`:

```
kira: heap allocated=3 freed=0 live=3 retained=0 imbalance=+3
```

Closing a receiver already drained them; ending the run did not. The table is
now told at creation whether a queued word is a token — by then there is no
send left to ask — and hands the words back when it is emptied. The VM
releases them on every path out of a run, trap included; the native half does
it in `kira_rt_channel_reset`.

**What the fix taught, which the finding did not say.** The generated entry
started the channel scope and never ended it, and ending it in `main` was not
enough: the table is thread-local, and under a native event loop the thread
`@Main` runs on is not the one `main` returns on. A reset in `main` emptied a
table nothing had put anything in — as quiet as no reset at all, and it looked
like a fix. The scope now ends in the function that started it.

## 10. Macros were visible without an import — FIXED

**Verified by running it.** An application importing `Outer`, which imports
`Inner`, calling `innerDouble!(21)` from `Inner` printed `42`. The same file's
ordinary function was correctly refused with `KSEM061`. One rule, two answers.

Macros expand before the analyzer's import table exists, so the imports are
read off the token stream and `ImportTable::sees` answers the question — the
same implementation, so a macro's visibility cannot drift from a function's.

A file that can see none of a program's macros still runs the expansion pass.
Skipping it would leave `name!(…)` in the text with nothing to say why, so the
shortcut that keeps a macro-free program byte-identical now asks about the
program rather than about the file.

## 11. A shadowed macro survived in the other kind maps — FIXED

The three kinds are three maps and lookups are by kind, so a claim that wrote
only its own kind left the shadowed declaration reachable through another: an
application's declarative `Name` took the name while `@Name` still ran a
dependency's attribute macro. The regression test was checked against the old
code and fails on it.

## 12. `registry.rs` at 1,082 lines — FIXED

Objectively true and the ceiling is unconditional. Split into what a
declaration says, how one is read off a token stream, and what a program does
with them: 291, 145, 566 and 208 lines. The visibility work above had taken it
to 1,174 before the split.

## 13. Pointer payloads only refused at the top level — FIXED

**Verified by running it.** `Channel<RawPtr>()` is refused; `struct Envelope {
let pointer: RawPtr }` and `Channel<Envelope>()` was accepted. The same
address, one wrapper away from the rule. The question is now asked of the
whole value — structs, arrays, enum payloads, distincts, cells — following the
same walk that decides whether the native-state store can hold the type, and
the refusal names the pointer it found.

Filed P2 by the review. It is the one of the six that no program in the
repository could have hit, because nothing declares a pointer-bearing payload;
it is also the one that would have been silent rather than loud if it had.

# Third review round, on `730b51f`/`05d91f2`: thirteen findings

Ten from @codex and four from CodeRabbit, with one overlap. Every one was
checked by running it first. Every one was right — the first round's seven
contained two that were not, and this round contained none, which is worth
noting rather than assuming either way next time.

## 14. The runtime ABI was not bumped — FIXED, and the finding holds me to my own rule

The criterion I argued for when I refused a bump for an appended opcode was
that appending changes no signature, while adding a `kira_rt_*` symbol with a
new signature is exactly what the marker guards. Three of my changes are on the
wrong side of that line: a new required symbol, `Create` reading an operand it
used to ignore, and `kira_rt_channel_reset` freeing storage it used to drop.
The constant's own doc says to bump on any change "to what a helper owns or
frees", which the reset is.

`RUNTIME_ABI_VERSION` 15 → 16, marker `kira_rt_abi_version_16`. **The guard
proved itself rather than being assumed**: with the compiler rebuilt and the
archive stale, a build was refused by name —

```
kira: the native runtime archive ... was built against a different version of
the runtime ABI (it does not define `kira_rt_abi_version_16`)
```

## 15. A hybrid *library* still kept two channel tables — FIXED

Both reviewers named this line independently. `SeamHost` holds the loaded
`NativeLibrary` and uses it for `call_native`, but forwarded `channel_op` to
the consumer's host — which has no table, so the bytecode half fell back to its
own private one while native code used the archive's. My earlier fix covered
the application session and I applied the forwarding here mechanically, which
is the "covers one path and not its siblings" shape.

**And a second one neither reviewer named.** That host implements no callback
state at all, so on this surface `native_state_create` answered `NoStateHost`
and a payload that owns storage could not cross the seam. Both now reach the
loaded half.

## 16. Teardown reached two entry paths of five — FIXED

`run_entry` and its debug variant were covered. A fresh VM with its own channel
table is also built for `Program::call_capturing`, `Program::call_state`, a
nested re-entry on a shared heap, and every call on a persistent `Instance` —
the last being the surface a hybrid library runs on. All release now, and all
before the outcome is unwrapped, so a run that trapped gives back what it
queued.

## 17. The heap report ran before the channel scope ended — FIXED

CodeRabbit, and the sharpest of the four: on a target with no native event loop
the generated entry asked the heap to balance and *then* released what the run
never delivered, so a clean program could be reported leaky.

**It does not invalidate the earlier measurements, and I checked rather than
assumed.** Every program I measured takes the event-loop shape, where the reset
is in the helper and the report is in `main` after the loop returns — the
emitted IR shows the reset first. The `imbalance=+3` before and `+0` after both
stand. The ordering was still wrong in the other branch, and the reset now sits
immediately after `@Main` returns, before anything releases or reports.

## 18. Macro shadowing was resolved before visibility was known — FIXED

**Verified by running it.** Two files of one application, each importing a
different package, both packages declaring `dupHelper!`: the later `absorb`
took the name globally and was then filtered out for the file that imports the
earlier one, so a valid call was answered `KMAC001: is not a macro`.

Declarations are now kept per name in file order, one tagged declaration
covering all three kinds, and the winner is picked *after* filtering. That also
subsumes the previous round's cross-kind eviction, which was the same question
asked one step too early.

## 19. A newline did not make an `import` top-level — FIXED

**Verified by running it.** A macro template containing a line reading
`import Inner` handed the declaring file `Inner`'s macros with no import: the
visibility hole one level down from the one this scanner exists to close, and
the template is text the macro pastes rather than the file importing anything.
Brace depth settles it, which is what the declaration scanner beside it already
does.

## 20. The import reading discarded `as` aliases — FIXED

**Verified by running it.** `import Inner as Shared` followed by
`import Outer as Shared` binds the root once, so `Inner` is gone — and an
ordinary name from it is correctly refused, while `innerDouble!(21)` expanded
and printed 42. The reading synthesised each root from the path's last segment
and so believed both packages imported. It carries the written alias now, and
both answers agree.

## 21. Three malformed-macro paths refused silently — FIXED

CodeRabbit, and it is the rule I recorded myself after the "looks fine, is
empty" family. Three `?` returned `None` with no diagnostic, and `collect_file`
then blanked the rest of the file. **Verified by running it**: a `comptime
macro` with an unclosed `expand(` list deleted everything after it, including
`@Main`, and the only message was `KSEM011: program has no @Main function to
run`. Each now names the macro and the shape it could not close.

## 22, 23. Two files over the ceiling — FIXED

`channels.rs` 723 → 536 with its tests beside it; the macro registry, which my
own visibility work had taken to 1,174, into 291/145/566/208.

## 24, 25, 26. Three behaviours covered from Rust only — FIXED

AGENTS.md is unambiguous: every Kira behaviour belongs in `tests-kik`. Three
new packages, because the behaviours are three different shapes:

- `macro-imports` runs, and every file in it reaches a dependency's macro
  through its own import, one under an alias.
- `refusals` must not build. A harness of runnable cases cannot hold a refusal,
  so the pointer-bearing payloads and the transitive macro call live in one
  package that fails, with the codes pinned by count — a rule that stopped
  applying to three of four nested shapes would still report one.
- `channel-teardown` ends a run with payloads queued. It cannot be a `Test`
  construct: it prints the same bytes and exits zero whether the storage came
  back or not, so it is asserted on the runtime's heap balance, and on the
  allocation count beside it because a case that allocated nothing balances
  trivially. **It reports `imbalance=+5` against the old code**, checked by
  removing the fix rather than by reasoning.
