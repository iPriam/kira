# FFI seam: what has actually been run

Observed output, not expectation. Each line is a command that finished on the
machine named beside it. Anything absent from this file has not been run.

Host: `c8g.medium`, one Graviton4 vCPU, 1.8 GB, aarch64-linux-gnu, LLVM 23.1.0.
A spot instance, so a suite is run one step at a time and its result recorded
here before the next one starts.

## Unit suites

| Suite | Result |
| --- | --- |
| `cargo test -p kira-runtime-abi` | **175 passed, 0 failed** |
| `cargo test -p kira-semantics` | **851 passed, 0 failed** |

The seam's `Bool` rule is stated once in `kira_runtime_abi::c_storage` and the
two cases that pin it both ran: `a_bool_crosses_as_exactly_one_or_zero` and
`every_nonzero_byte_reads_back_as_true`, the second over all 255 nonzero bytes.

## Not runnable on this host

`emcc` and `node` are absent, so the seven wasm end-to-end tests cannot run. The
wasm coverage added in `9894393` — the `_Bool` byte and `RawPtr.null` in
`ffi_program_scalar.kira` — is committed but **unexecuted**, and nothing here
should be read as having verified it.

The four declaration rules and the pointer surface ran by name:
`an_address_declaration_answers_a_pointer_and_nothing_else` (KSEM369),
`two_declarations_of_one_symbol_must_agree` (KSEM370),
`retains_names_a_parameter_that_holds_c_storage` (KSEM371),
`raw_ptr_has_no_member_but_null` (KSEM368), and the six `raw_pointers` cases
covering the constant, comparison, `distinct` identity, shadowing, and the two
refusals a pointer keeps (no ordering, no comparison to an integer).

### One existing test the new rules moved

`drop::a_retained_foreign_argument_may_not_run_a_body` pins KSEM305: a
`retains:` argument may not be a type that runs a user `Drop`, because the
retained registry frees at teardown with no engine left to enter the body. Its
carrier was a `@FFI.Struct` with one `I32` field, which crosses the seam **as
that field** rather than as an aggregate — a C handle struct passed in a
register. There is no storage in it for a callee to keep, so it is now KSEM371
and never reaches the rule under test. The carrier gained a second field, which
makes it a real C-layout aggregate; the assertion is unchanged.
