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
