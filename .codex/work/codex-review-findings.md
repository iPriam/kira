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

## 4. Parsing the minimum Int overflows — see below

## 5. Same-line handler payloads missed by the hygiene fix — FIXED

The claim: `handle { TooBig(reason) { … } }` on one line leaves the payload
unrenamed, because the block form asked for a line break.

Correct, and a hole in the fix it reviewed. A handler arm begins a statement,
and a statement begins after a brace as well as after a line break — the arm
list's own `{`, or the `}` that closed the arm before it. That is what tells it
from `if ready(flag) { … }`, whose call follows a keyword. Two unit tests, one
for the single-arm form and one for two arms sharing a line, and a harness
construct for the same-line spelling specifically.

## 6, 7. File-size ceiling violations — see below
