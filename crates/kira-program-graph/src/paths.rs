//! Path canonicalization and existence helpers.
//!
//! Ported from kira-zig `kira_program_graph/src/paths.zig`. Owns
//! `canonicalizeExistingPath`, `canonicalizeSourceRoot`,
//! `canonicalizeDirectory`, `absolutizeLexical`, `bindingsRootForSourceRoot`
//! (generated FFI bindings live in `<package>/bindings`, a sibling of `app/`),
//! `fileExists` / `dirExists`, and the case-insensitive-on-Windows
//! `pathWithinRoot` / `pathEql` component-boundary checks.

// TODO(port): the helpers above over `std::path` / `std::fs`.
