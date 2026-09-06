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

`kira test --backend vm tests-kik/harness`, run against this branch's Foundation
with a compiler built from this branch:

```
1475 passed, 0 failed, 0 skipped, 1475 total
```

All ten `Dgx` cases ran and passed. `crates/kira-cli/tests/kik_harness.rs` pins
1475, up from 1465.

The run reaching those cases at all is the proof that the generated Foundation
was the one in use: `DgxCodesTheHandWrittenRegistryMissed` names `.KPAR043`,
`.KSEM255`, `.KSEM312` and five more that the hand-written `KiraError` never
declared, so the old Foundation could not have compiled the harness.

`knvm binstall --debug` cannot complete on this host. It cross-builds the
runtime archive for `wasm32-unknown-emscripten`, that target is not installed,
and there is no emsdk, so it fails after the host build with `E0463: can't find
crate for std`. The harness was therefore run the way
`crates/kira-cli/tests/kik_harness.rs` runs it, with `KIRA_FOUNDATION_HOME`
naming the checkout's `foundation/` rather than an installed toolchain's. That
test pins Foundation deliberately: an installed toolchain's Foundation is not
the one under test.
