---
name: wire-formats
description: "The append-only serialized contracts in this workspace — KBC1 bytecode, KHM1 hybrid manifests, opcodes, BridgeValue, the kira_rt_* symbols and the RUNTIME_ABI_VERSION marker — what may change, what may never, and how a decoder must behave. Read before touching an opcode, a tag, a #[repr(C)] type, a serialized field, or a kira_rt_* signature."
---

# Wire formats

Treat opcodes, KBC/KHM magics, serialized tags, and wire enums as
**append-only**. Never renumber, reorder, or insert mid-enum. Append a variant
at the end safely; everything else breaks artifacts that already exist.

## The contracts

| Contract | Defined in | Shape |
|---|---|---|
| `KBC1` module | `kira-bytecode/src/module.rs` | bytecode module: functions, main, string pool |
| Opcodes | `kira-bytecode/src/op.rs` | append-only; a new op goes on the end |
| `KHM1` manifest | `kira-hybrid-definition/src/manifest.rs` | payload paths, entry, per-function engine + signature + symbol |
| `KLB1` bundle | `kira-live/src/bundle.rs` | the runner artifact boundary: runner + profile + entry, one row per payload (name, kind, SHA-256, size) |
| `KLP1` protocol | `kira-live/src/protocol.rs` | live server/runner messages: length-prefixed frames, one tag byte per message |
| `BridgeValue` | `kira-runtime-abi/src/bridge.rs` | `{ u8 tag, [7]u8 reserved, u64 payload }`, 16 bytes |
| `Execution` / `Ownership` | `kira-runtime-abi/` | wire bytes, append-only |
| `kira_rt_*` | `kira-native-bridge/src/runtime.rs` | native runtime: print, strings, div-zero trap |
| ABI marker | `kira-runtime-abi/src/lib.rs` | `RUNTIME_ABI_VERSION` / `RUNTIME_ABI_MARKER` |

`RunnerId`, `BuildProfile`, and `SessionPhase` have wire bytes too. The first two
take theirs from `RunnerId::index`/`BuildProfile::index` in `kira-manifest`, so
**reordering that matrix is a wire-format change** even though nothing in
`kira-manifest` says so — bundles already written decode by those numbers.
`kira-live`'s pinning tests (`runner_wire_bytes_are_pinned`,
`profile_wire_bytes_are_pinned`, `phase_wire_bytes_are_pinned`) spell the bytes
out literally and are what turns a reorder into a failing test rather than a
silent redirection.

## Adding versus changing

- **Adding** a `kira_rt_*` helper, an opcode, or a tag at the end is
  append-only and needs no version bump.
- **Changing** a `kira_rt_*` signature, what a helper owns or frees, or how a
  value is represented at the native ABI bumps `RUNTIME_ABI_VERSION` — which
  means renaming the marker function in `runtime.rs`, because the *name* is the
  guard. The test in that file fails otherwise.
- Anything `#[repr(C)]` changes only together with the layout test in the same
  file.

## Why the marker exists

Generated native code and the runtime archive are built separately and linked
together. If they disagree, the symbols still resolve by name and the mismatch
is silent: the program calls the old code under the new ABI and corrupts
memory. That was a real failure here, caught only by a Rust debug-only UB
check a release build would not run. The version is baked into a symbol name
the backend emits a reference to, so a stale archive fails the **link**, by
name, instead of the program failing at run time.

## Decoders validate, they don't trust

A `Module` and a `HybridManifest` are public, deserializable artifacts: anyone
can hand Kira a malformed one. So a decoder returns a typed error for every
malformed input and panics on none — every truncation, every unknown byte,
every out-of-range index. The round-trip and truncation tests next to each
format are the guard, and a new field means extending them in the same change.

A byte foreign code can write is a transparent newtype with associated consts
(`BridgeValueTag`), never a Rust `enum` — an out-of-range discriminant in a
Rust `enum` is UB. Unknown tags decode to `None` and are rejected, never
guessed.