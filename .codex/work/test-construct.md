# `Test`, end to end

`kira test` runs a Kira suite on vm, llvm, and hybrid. A case is a `Test`
declaration, `Test` is an ordinary construct family declared in Foundation, and
**nothing in the compiler names it**. What the compiler gained is two things
that are generic over any family and any program: a way to enumerate a
program's declarations, and a way to compare two `Any` values.

```
$ kira test suite/
ok   SumsToTen
FAIL AlsoFails
ok   GreetsByName
2 passed, 3 total
```

## The missing capability was enumeration, not declaration

A case was always writable — `foundation/app/Test.kira` declares the family, and
`kira check` accepted a suite before any of this work. What no macro could do
was *find* the cases. Every procedural form is attached to the site it rewrites
(`function`, `attribute`, `derive`, `wrapper`), so none of them can answer "what
does this program contain?".

`collector` is that form. It runs once for the whole program and is handed every
declaration in it. Its selection is a string compare — `d.family == "Test"` —
against `Declaration.family`, the construct family a declaration was written in.
A library can declare its own family and its own collector and get identical
treatment with no compiler change.

Two scanner sites had to learn the parenthesis-free spelling `Family Name { … }`
that a family with no construction inputs uses; both previously required
`Family Name(` and so skipped every `Test` declaration silently.

## Where a collector's output goes

Into the entry file, appended to its expanded text. A collector has no site to
splice into, and giving it a file of its own would mean minting a `SourceId`
after the program's files are fixed.

This is the part that needed care. Expansion is memoized per file
(`expanded_file`), and a collector's answer is a function of the whole program,
so it cannot live there. It is a separate whole-program query, `collected`, and
the entry file's parse goes through `parsed_entry` rather than `parsed_file`
when the program has one. A program with no collector never reaches that path
and keeps the per-file memoization it always had.

The first attempt appended to `ExpandedProgram.entry` alone, which is consumed
only for diagnostics — parsing reads `expanded_file` per file. The collector ran
and its output was computed and discarded. Worth remembering: `expanded` is not
what gets parsed.

## `kira test`

The generated entry is `kiraTestMain`, and that name is the whole agreement
between Foundation and the CLI. `kira test` compiles the package, retargets
`ir.main` to that function, and runs it on the selected backend — so every
backend sees an ordinary program whose entrypoint happens to be the runner.

Compiling needed a third `BuildKind`. `Application` demands an `@Main` and
`Library` refuses one; a suite needs neither rule. `BuildKind::Test` accepts an
`@Main` when written and does not require one, so a package that is both an
application and a suite keeps both entrypoints and `kira run` still runs the
application.

A program that imports Foundation but declares no case runs an empty suite and
exits zero — "no tests here" is an answer, not a build failure. A program that
never imports Foundation generates no runner at all and is told so by name.

## What it rests on

`Any == Any` (see [any-equality.md](any-equality.md)). A `Test` member returns
`Any` and its expectation arrives as `Result<Any, TestFailure>`, so a runner
written in Kira cannot compare the two without it. The comparison is structural
and nominal, which is why a case may answer with whatever it measures — an
`Int`, a `String`, a struct — and why two values of different types are unequal
rather than an error.

## Not done

The exit code does not yet reflect failures: a run with a failing case prints
`FAIL` and exits zero, so CI cannot gate on it. The runner would need to return
a count and `kira test` to use it as the process status.

`TestFailure.Diagnostic` and `.Compile` are declared and unused — a case that
should fail to compile has no way to say so yet, which is what the oracle's
fail-corpus needs.

## Verification

2496 tests pass, clippy clean under `-D warnings`, `cargo fmt --check` clean.
Five end-to-end cases in `kira-cli/tests/end_to_end/tests_verb.rs` drive the
real binary: a suite with no `@Main`, a failing case, cases answering with
`String`/`Bool`/a struct, an empty suite, and a program without Foundation.
They pin `KIRA_FOUNDATION_HOME` to this checkout — the other end-to-end modules
deliberately test the *installed* Foundation, and these are about the runner
this tree ships.
