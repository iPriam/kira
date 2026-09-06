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

The seam's `Bool` rule is stated once in `kira_runtime_abi::c_storage` and the
two cases that pin it both ran: `a_bool_crosses_as_exactly_one_or_zero` and
`every_nonzero_byte_reads_back_as_true`, the second over all 255 nonzero bytes.

## Not runnable on this host

`emcc` and `node` are absent, so the seven wasm end-to-end tests cannot run. The
wasm coverage added in `9894393` — the `_Bool` byte and `RawPtr.null` in
`ffi_program_scalar.kira` — is committed but **unexecuted**, and nothing here
should be read as having verified it.
