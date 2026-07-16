---
name: verifying-work
description: "The done-bar for this workspace: the four CI gates, both LLVM feature paths, the wasm32 portable-core check, the backend parity suite, and why a locally-green LLVM build proves less than it looks. Read before claiming any change is done, and before committing."
---

# Verifying work

Reject fake success — only Kira-owned code paths emit Kira success markers.
Never accept smoke surfaces, placeholders, hardcoded `return true`,
host-rendered content, or "the app launched so it works" as proof.

## Trust the local build least

CI runs on a machine with **no LLVM**. A dev machine here carries a managed
LLVM at `~/.kira/toolchains/llvm/<version>/<host>`, so a build that passes
locally may be passing *because of the machine* rather than because of the
change. Weigh that against what it already cost the sibling repo: a local
build went green, the release was tagged, and CI failed on all three platforms
— the workflow never provisioned LLVM at all.

So run **both** feature paths, and count only the one CI has:

```sh
# The path CI has. This is the gate.
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
cargo fmt --check

# The path only this machine has. Extra, never a substitute.
export LLVM_SYS_221_PREFIX=~/.kira/toolchains/llvm/22.1.4/aarch64-macos
cargo clippy --workspace --all-targets --features kira-cli/llvm -- -D warnings
cargo test --workspace --features kira-cli/llvm
```

Treat anything touching toolchain provisioning, dynamic linking, or a thing a
fresh checkout would not have as unverified until it is confirmed somewhere
without it pre-installed.

## Clear the rest of the bar

- **Portable core.** Confirm `kira-vm-runtime` and everything below it still
  compiles for wasm:
  `cargo check -p kira-vm-runtime --target wasm32-unknown-unknown`.
- **Parity.** Prove it rather than asserting it: run
  `crates/kira-cli/tests/backend_parity.rs` for any lowering or semantics
  change — same program, same stdout, same exit status on VM and native. Reject
  VM-only passing for backend-sensitive work, and prefer many small cases over
  one broad one.
- **Layout and marker tests.** Run `crates/kira-runtime-abi/src/bridge.rs` if
  `BridgeValue` moves at all, and `crates/kira-native-bridge/src/runtime.rs` if
  any `kira_rt_*` signature changes.
- **Stale runtime archive.** Remember that `cargo build -p kira-cli` refreshes
  that crate's rlib but **not** `kira-native-bridge`'s staticlib, so the
  archive beside `kirac` can predate the compiler linking it. Use
  `cargo build --workspace` to cover both.

## Report what actually ran

State what you ran and what it said. Report a skipped test as skipped and a
failure with its output. Reject "should work", "builds clean", and a green
build standing in for a behavior claim. On finding a fake-success path, add a
negative test proving it cannot pass again.