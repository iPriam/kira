# Diagnostics registry: what was run

Recorded because the claim "the drift gate fails when the registry drifts" is
worth nothing unless someone watched it fail. Host: c8g.medium, one vCPU, cold
build.

## Counts

Measured against `41e1169`, the commit before the registry was generated:

| | |
|---|---|
| `KiraError` variants, hand-written | 290 |
| `kiraErrorFromCode` arms, hand-written | 287 |
| codes the toolchain emits | 438 |
| in both | 129 |
| emitted, the enum missed | 309 |
| the enum named, nothing emits | 161 |
| in the enum with no arm in the lookup | `KLEX004`, `KLEX005`, `KLEX006` |

The last row is the sharpest one: `.KLEX005` existed as a value, so a test could
name it, and `kiraErrorFromCode("KLEX005")` still answered `.Unrecognized`.

## The four gates, each watched failing

`cargo test -p kira-diagnostic-registry --test registry`.

A variant deleted from `Diagnostics.kira`, an arm deleted from
`DiagnosticCodes.kira`, and a row edited in `codes.mdx`:

```
test every_generated_artifact_is_current ... FAILED
these generated files no longer match the code table:
foundation/app/Kira/Diagnostics.kira, foundation/app/Kira/DiagnosticCodes.kira,
sites/docs/content/docs/appendix/diagnostics/codes.mdx
```

`KSEM107` deleted from the table and `KIC002` invented in it:

```
test every_code_the_toolchain_emits_is_registered ... FAILED
these codes are emitted but not registered: KSEM107 (crates/kira-semantics/src/ownership.rs)

test every_registered_code_is_one_the_toolchain_emits ... FAILED
these codes are registered but nothing emits them: KIC002

test the_documentation_names_no_unregistered_code ... FAILED
the documentation names codes the table does not list: KSEM107 in codes.mdx,
index.mdx, language-guide/functions.mdx, language-guide/memory-and-safety.mdx,
language-reference/about-the-language-reference.mdx, welcome.mdx
```

A row out of family and number order never reaches a test; `build.rs` refuses it:

```
error: kira-diagnostic-messages@1.8.3: diagnostic-codes.tsv line 452:
KIC001 does not follow the row before it in family and number order
```

Restored and green again: 5 passed, plus 19 unit tests in
`kira-diagnostic-messages` and 10 in `kira-diagnostic-registry`.

## Harness

`tests-kik/harness` holds 1475 `Test` constructs at this branch, 10 of them
`DgxDiagnosticCodeTests.kira`. `crates/kira-cli/tests/kik_harness.rs` pins that
number.

A `foundation/` edit is invisible to `kira test` until `knvm binstall --debug`,
and the installed dev toolchain was still carrying the hand-written
`Diagnostics.kira` when this was written. Run the binstall before believing a
harness result.
