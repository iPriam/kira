---
name: wire-formats
description: "Read before changing serialized tags, opcodes, wire enums, `#[repr(C)]` layouts, `kira_rt_*` signatures, or explicit byte mappings."
---


Append new opcodes, tags, fields, and explicit byte values. Never renumber, reorder, reuse, or insert between existing values.

Treat changes to `RunnerId::index`, `BuildProfile::index`, `SessionPhase::as_byte`, and `ReloadMode::as_byte` as wire-format changes. Keep byte-pinning tests passing.

Bump `RUNTIME_ABI_VERSION` and rename its marker function when changing a `kira_rt_*` signature, ownership contract, or native representation.

Update the colocated layout test when changing a `#[repr(C)]` type.

Reject malformed, truncated, unknown, and out-of-range input with typed errors. Never panic or guess. Add round-trip and truncation coverage with each format change.

Represent bytes written by foreign code as transparent newtypes with associated constants, not Rust enums.