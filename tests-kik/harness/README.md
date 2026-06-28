# Kira in-depth stress harness

A single monolithic Kira app that exercises the executable surface of the
language in depth. It serves two complementary roles:

1. **`kira test` suite** — 949+ `Test` declarations (the Foundation `Test`
   construct) asserting concrete computed values. Run with `kira test
   tests-kik/harness`. Each test reduces a feature exercise to a scalar
   (`Int`/`Bool`/`String`) and asserts a hand-computed expected value via
   `Result.Ok(...)`; failure/trap paths are asserted with
   `Result.Error(TestFailure.Runtime(""))` (which passes iff the body traps).
2. **`@Main` parity/leak run** — `main.kira` prints deterministic per-area
   checksums; running it across vm/llvm/hybrid and diffing proves backend
   parity, and `KIRA_RUNTIME_MEMORY_REPORT=1` proves the run is leak-clean.

It is designed to surface memory problems (leaks, double frees, use-after-free)
and backend-parity bugs that the smaller per-feature kik suites miss.

## Authoring `Test` declarations (gotchas)

`app/` is ONE flat package, so:

- Do NOT `import Foundation` outside `main.kira` (a second import is a duplicate
  top-level name, KSEM003). `Test`/`Result`/`TestFailure` are package-wide.
- Every top-level name (function/struct/enum/class/Test) must be globally unique;
  the `*Tests.kira` files use per-domain prefixes (`sct`, `Cfx`, `Colx`, `Stx`,
  `Enx`, `Strx`).
- The runner compares ONLY scalar values — struct/array/enum equality is NOT
  structural. Reduce every test to an Int/Bool/String.
- The `test` block must end with a clean trailing `return <scalar>`; returning
  only from inside `match`/`if` arms infers the type as Void (KSEM031). Use a
  `var out` accumulator.

## What it covers

`app/` is one package; each `*.kira` area file (and its `*Tests.kira` companion)
stresses one area in depth:

- `Scalars.kira` — integer/float arithmetic, comparisons, boolean short-circuit,
  conditional expressions, `switch`, recursion (factorial/fibonacci/gcd).
- `Collections.kira` — array literals, `append` in tight loops, indexing,
  `for`-in iteration, structs with owned array fields, arrays of structs.
- `Structs.kira` — nested structs, methods with `self`, `borrow mut` writeback,
  value-copy independence, deep nesting churned in loops.
- `Enums.kira` — payload-less and single-payload variants, exhaustive `match`
  with payload binding, generic `Result<T, E>` matching.
- `Closures.kira` — function types, named references, inline literals,
  higher-order functions, immutable by-value captures, shared mutable `var`
  captures.
- `Ownership.kira` — `move` into consuming functions, owned aggregates created
  and dropped in tight loops, struct-with-owned-array churn.

## How to run

```sh
# Value assertions: run the Test-construct suite (VM). Must be 0 failed.
kira test tests-kik/harness

# Cross-backend parity: the printed checksums must be byte-identical.
kira run --backend vm     tests-kik/harness
kira run --backend llvm   tests-kik/harness
kira run --backend hybrid tests-kik/harness

# Leak detection: every "current=" count in the report must be 0 at exit.
KIRA_RUNTIME_MEMORY_REPORT=1 kira run --backend vm     tests-kik/harness
KIRA_RUNTIME_MEMORY_REPORT=1 kira run --backend hybrid tests-kik/harness
```

The harness output is currently identical across vm/llvm/hybrid and the VM run
is leak-clean. Known open bugs it surfaced are documented in `FINDINGS.md` with
runnable reproductions under `known-bugs/`. The harness deliberately uses idioms
that are correct on all three backends so it stays green as a forward regression
asset; the buggy idioms live in `known-bugs/` so they can be promoted to harness
coverage once fixed.
