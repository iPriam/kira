//! What the generator emits, asserted on directly, one engine per file.
//!
//! The generated crates are compiled for real by `kira-export-consumer`, which
//! is the proof that they work. These tests are the complement: they pin the
//! decisions a compiler cannot check — that a stale-build guard is present at
//! all, that a keyword name is escaped rather than renamed, that an unused
//! import is never emitted — each of which would otherwise fail silently or in
//! somebody else's crate.

// Explicit paths: this file is itself reached by `#[path]` from `mod.rs`, so
// its module directory is `wrapper/` rather than `wrapper/tests/`.
#[path = "tests/hybrid.rs"]
pub(super) mod hybrid;
#[path = "tests/native.rs"]
pub(super) mod native;
#[path = "tests/vm.rs"]
pub(super) mod vm;
