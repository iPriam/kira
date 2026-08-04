---
name: verifying-work
description: "The done-bar for this workspace: the four CI gates run with the managed LLVM, the wasm32 portable-core check, the backend parity suite, and the provisioning contract every builder depends on. Read before claiming any change is done, and before committing."
---

# Verifying work

Reject fake success — only Kira-owned code paths emit Kira success markers.
Never accept smoke surfaces, placeholders, hardcoded `return true`,
host-rendered content, or "the app launched so it works" as proof.

## The gates, and the LLVM they stand on

The LLVM backend is a hard dependency: nothing in this workspace builds
without the managed LLVM, and there is no feature flag to fall back to. The
backend's own build script discovers the bundle the repo-root
`llvm-metadata.toml` pins — `KIRA_LLVM_HOME` override first, then
`~/.kira/toolchains/llvm/<version>/<host>` — so every builder (a dev shell,
CI, a release job, `knvm binstall`) runs the gates with no environment setup:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo nextest run --workspace   # cargo test --workspace works, serially
```

CI provisions that same bundle from the release `llvm-metadata.toml` names
before building, so its gates and a local run prove the same configuration.
The bundles themselves are published by the `llvm-bundles` workflow, run once
per pin move — until it has run for the current pin, no release exists and
every CI provisioning step fails by name. That failure means "publish the
bundles", never "work around the pin".

Treat anything touching toolchain provisioning, dynamic linking, or a thing a
fresh checkout would not have as unverified until it is confirmed somewhere
without it pre-installed.

## Clear the rest of the bar

- **Portable core.** Confirm `kira-vm-runtime` and everything below it still
  compiles for wasm:
  `cargo check -p kira-vm-runtime --target wasm32-unknown-unknown`.
- **Lint.** Prove a change to `kira lint` or to Foundation's `LintRunner.kira`
  against a package that *has* a `linter.kira`, pinning this checkout:
  `KIRA_FOUNDATION_HOME=$PWD/foundation cargo run -p kira-cli -- lint
  ../ui-foundation`. Without the pin the lints come from whichever toolchain is
  installed rather than from here. Expect `foundation/` itself to have no
  `linter.kira`: a `Lint` entry needs `import Foundation`, which inside
  Foundation is a self-import that resolves to nothing.
- **Parity.** Prove it rather than asserting it: run
  `crates/kira-cli/tests/backend_parity/` for any lowering or semantics
  change — same program, same stdout, same exit status on VM and native. Reject
  VM-only passing for backend-sensitive work, and prefer many small cases over
  one broad one.
- **Layout and marker tests.** Run `crates/kira-runtime-abi/src/bridge.rs` if
  `BridgeValue` moves at all, and `crates/kira-native-bridge/src/runtime.rs` if
  any `kira_rt_*` signature changes.
- **Stale runtime archive.** Remember that `cargo build -p kira-cli` refreshes
  that crate's rlib but **not** `kira-native-bridge`'s staticlib, so the
  archive beside `kira` can predate the compiler linking it. Use
  `cargo build --workspace` to cover both.

## Report what actually ran

State what you ran and what it said. Report a skipped test as skipped and a
failure with its output. Reject "should work", "builds clean", and a green
build standing in for a behavior claim. On finding a fake-success path, add a
negative test proving it cannot pass again.